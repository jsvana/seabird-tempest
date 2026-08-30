use std::collections::{BTreeMap, HashMap};
use std::env;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use seabird::proto::{ChannelSource, CommandEvent, CommandMetadata, StreamEventsRequest};
use seabird::{ClientConfig, SeabirdClient};
use serde::Deserialize;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Deserialize)]
struct LatestEntry {
    value: f64,
    age_secs: i64,
}

#[derive(Clone)]
struct TempestConfig {
    url: String,
    token: String,
}

async fn fetch_latest(
    client: &reqwest::Client,
    config: &TempestConfig,
) -> Result<HashMap<String, LatestEntry>> {
    let response = client
        .get(format!(
            "{}/api/v1/latest",
            config.url.trim_end_matches('/')
        ))
        .bearer_auth(&config.token)
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json().await?)
}

/// (low, high) of a house's temperature over the last 24 hours, from the
/// aggregator's distribution endpoint. None when there are no samples.
async fn fetch_temperature_range(
    client: &reqwest::Client,
    config: &TempestConfig,
    house: &str,
) -> Result<Option<(f64, f64)>> {
    #[derive(Deserialize)]
    struct Range {
        min: f64,
        max: f64,
    }

    let response = client
        .get(format!(
            "{}/api/v1/distributions/{house}.temperature?window=24h",
            config.url.trim_end_matches('/')
        ))
        .bearer_auth(&config.token)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let range: Range = response.error_for_status()?.json().await?;
    Ok(Some((range.min, range.max)))
}

/// Group `house.metric` entries by house. Metrics without a `.` (the legacy
/// unprefixed names, test junk, etc.) are ignored.
fn group_houses(
    latest: HashMap<String, LatestEntry>,
) -> BTreeMap<String, BTreeMap<String, LatestEntry>> {
    let mut houses: BTreeMap<String, BTreeMap<String, LatestEntry>> = BTreeMap::new();
    for (metric, entry) in latest {
        if let Some((house, name)) = metric.split_once('.') {
            if !house.is_empty() && !name.is_empty() {
                houses
                    .entry(house.to_string())
                    .or_default()
                    .insert(name.to_string(), entry);
            }
        }
    }
    houses
}

/// Which houses a `tempest` invocation should print, or the hint to send back
/// when nothing matched.
#[derive(Debug, PartialEq, Eq)]
enum Selection {
    Houses(Vec<String>),
    Hint(String),
}

fn select_houses(
    arg: &str,
    nick: Option<&str>,
    houses: &BTreeMap<String, BTreeMap<String, LatestEntry>>,
) -> Selection {
    let all = || Selection::Houses(houses.keys().cloned().collect());

    let name = match arg.split_whitespace().next() {
        Some(name) if name.eq_ignore_ascii_case("all") => return all(),
        Some(name) => name,
        // A bare `tempest` reports the requester's own house. Command events
        // should always carry a user, but without one there is no nick to match.
        None => match nick {
            Some(nick) => nick,
            None => return all(),
        },
    };

    match houses.keys().find(|house| house.eq_ignore_ascii_case(name)) {
        Some(house) => Selection::Houses(vec![house.clone()]),
        None => Selection::Hint(format!(
            "no house named {name} (houses: {}). Try !tempest all.",
            houses.keys().cloned().collect::<Vec<_>>().join(", ")
        )),
    }
}

fn format_age(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 90 {
        format!("{secs}s ago")
    } else if secs < 90 * 60 {
        format!("{}m ago", secs / 60)
    } else if secs < 36 * 3600 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Rain clause for a house line, or None when it isn't raining. `rain_rate` is
/// in/hr; below 0.01 it rounds to 0.00, so those report as a trace.
fn format_rain(rain_rate: Option<f64>) -> Option<String> {
    let rate = rain_rate.filter(|rate| *rate > 0.0)?;
    if rate < 0.01 {
        return Some("Trace rain".to_string());
    }
    let intensity = if rate < 0.1 {
        "Light"
    } else if rate < 0.3 {
        "Moderate"
    } else if rate < 2.0 {
        "Heavy"
    } else {
        "Torrential"
    };
    Some(format!("{intensity} rain, {rate:.2} in/hr"))
}

/// Freshness threshold beyond which a house line gets a "Last report" note.
const STALE_AFTER_SECS: i64 = 15 * 60;

fn format_house(
    house: &str,
    metrics: &BTreeMap<String, LatestEntry>,
    temperature_range: Option<(f64, f64)>,
) -> String {
    let value = |name: &str| metrics.get(name).map(|entry| entry.value);
    let mut sentences = Vec::new();

    if let Some(temperature) = value("temperature") {
        let mut sentence = format!("Currently {temperature:.1}°F");
        if let Some(feels_like) = value("feels_like") {
            sentence.push_str(&format!(", Feels Like {feels_like:.1}°F"));
        }
        sentences.push(sentence);
    }
    if let Some((low, high)) = temperature_range {
        sentences.push(format!("High {high:.1}°F, Low {low:.1}°F"));
    }
    if let Some(humidity) = value("humidity") {
        sentences.push(format!("Humidity {humidity:.0}%"));
    }
    if let Some(rain) = format_rain(value("rain_rate")) {
        sentences.push(rain);
    }

    if sentences.is_empty() {
        // A house pushing only metrics this bot doesn't format still shows up.
        sentences.push(format!("{} metrics reported", metrics.len()));
    }

    let age = metrics
        .values()
        .map(|entry| entry.age_secs)
        .min()
        .unwrap_or(0);
    if age > STALE_AFTER_SECS {
        sentences.push(format!("Last report {}", format_age(age)));
    }

    format!("{house}: {}.", sentences.join(". "))
}

fn with_reply(command_source: &ChannelSource, message: String) -> String {
    format!(
        "{}{}",
        command_source
            .user
            .as_ref()
            .map(|u| format!("{}: ", u.display_name))
            .unwrap_or_default(),
        message
    )
}

async fn handle_tempest(
    client: &mut SeabirdClient,
    tempest: &TempestConfig,
    command_source: ChannelSource,
    arg: &str,
) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let houses = match fetch_latest(&http, tempest).await {
        Ok(latest) => group_houses(latest),
        Err(err) => {
            error!("failed to fetch weather: {err:#}");
            client
                .send_message(
                    command_source.channel_id.clone(),
                    with_reply(&command_source, "failed to fetch weather".to_string()),
                    /* tags = */ None,
                )
                .await?;
            return Ok(());
        }
    };

    if houses.is_empty() {
        client
            .send_message(
                command_source.channel_id.clone(),
                with_reply(&command_source, "no weather data yet".to_string()),
                /* tags = */ None,
            )
            .await?;
        return Ok(());
    }

    let nick = command_source
        .user
        .as_ref()
        .map(|user| user.display_name.as_str());
    let selected = match select_houses(arg, nick, &houses) {
        Selection::Houses(selected) => selected,
        Selection::Hint(hint) => {
            client
                .send_message(
                    command_source.channel_id.clone(),
                    with_reply(&command_source, hint),
                    /* tags = */ None,
                )
                .await?;
            return Ok(());
        }
    };

    for house in &selected {
        let metrics = &houses[house];
        let range = match fetch_temperature_range(&http, tempest, house).await {
            Ok(range) => range,
            Err(err) => {
                error!("failed to fetch temperature range for {house}: {err:#}");
                None
            }
        };
        client
            .send_message(
                command_source.channel_id.clone(),
                format_house(house, metrics, range),
                /* tags = */ None,
            )
            .await?;
    }

    Ok(())
}

async fn process_event(
    client: &mut SeabirdClient,
    tempest: &TempestConfig,
    event: seabird::proto::Event,
) -> Result<()> {
    if let Some(seabird::proto::event::Inner::Command(CommandEvent {
        source: Some(command_source),
        command,
        arg,
    })) = event.inner
    {
        if command == "tempest" {
            info!("[cmd:tempest] {}", arg);
            handle_tempest(client, tempest, command_source, &arg).await?;
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let url = env::var("SEABIRD_URL").unwrap_or_else(|_| "https://api.seabird.chat".to_string());
    let token = env::var("SEABIRD_TOKEN").map_err(|_| anyhow!("must specify SEABIRD_TOKEN"))?;
    let tempest = TempestConfig {
        url: env::var("TEMPEST_URL").map_err(|_| anyhow!("must specify TEMPEST_URL"))?,
        token: env::var("TEMPEST_TOKEN").map_err(|_| anyhow!("must specify TEMPEST_TOKEN"))?,
    };

    let commands = HashMap::from_iter([(
        "tempest".to_string(),
        CommandMetadata {
            name: "tempest".to_string(),
            short_help: "latest weather at your house; <house> or all for others".to_string(),
            full_help: "Print the latest Tempest weather. With no argument, prints the house \
                        matching your nick. `tempest <house>` prints one house, `tempest all` \
                        prints every house reporting to the aggregator."
                .to_string(),
        },
    )]);

    // Reconnection loop with exponential backoff, mirroring seabird-ham.
    let mut reconnect_delay = Duration::from_secs(1);
    let max_reconnect_delay = Duration::from_secs(60);

    loop {
        info!("connecting with URL {}", url);

        let mut client = match SeabirdClient::new(ClientConfig {
            url: url.clone(),
            token: token.clone(),
        })
        .await
        {
            Ok(client) => client,
            Err(err) => {
                error!("failed to connect: {err}");
                warn!("reconnecting in {reconnect_delay:?}...");
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = std::cmp::min(reconnect_delay * 2, max_reconnect_delay);
                continue;
            }
        };

        reconnect_delay = Duration::from_secs(1);
        info!("connected. starting event stream...");

        let mut stream = match client
            .inner_mut_ref()
            .stream_events(StreamEventsRequest {
                commands: commands.clone(),
            })
            .await
        {
            Ok(response) => response.into_inner(),
            Err(err) => {
                error!("failed to start event stream: {err}");
                warn!("reconnecting in {reconnect_delay:?}...");
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = std::cmp::min(reconnect_delay * 2, max_reconnect_delay);
                continue;
            }
        };

        // Idle-watchdog to detect silently broken streams.
        let mut last_event = tokio::time::Instant::now();
        let mut watchdog = tokio::time::interval(Duration::from_secs(30));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let stream_result: Result<()> = loop {
            tokio::select! {
                next = stream.next() => {
                    last_event = tokio::time::Instant::now();
                    match next {
                        None => {
                            warn!("event stream ended");
                            break Ok(());
                        }
                        Some(Err(err)) => {
                            error!("event stream error: {err}");
                            break Err(err.into());
                        }
                        Some(Ok(event)) => {
                            if let Err(err) = process_event(&mut client, &tempest, event).await {
                                error!("error processing event: {err:#}");
                            }
                        }
                    }
                }
                _ = watchdog.tick() => {
                    if last_event.elapsed() > Duration::from_secs(600) {
                        error!("event stream timeout (no events for 10 minutes)");
                        break Err(anyhow!("stream timeout"));
                    }
                }
            }
        };

        match stream_result {
            Ok(()) => warn!("stream ended normally, reconnecting..."),
            Err(err) => error!("stream failed: {err}, reconnecting..."),
        }

        warn!("reconnecting in {reconnect_delay:?}...");
        tokio::time::sleep(reconnect_delay).await;
        reconnect_delay = std::cmp::min(reconnect_delay * 2, max_reconnect_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: f64, age_secs: i64) -> LatestEntry {
        LatestEntry { value, age_secs }
    }

    #[test]
    fn groups_by_house_and_skips_unprefixed() {
        let latest = HashMap::from_iter([
            ("jsvana.temperature".to_string(), entry(77.0, 30)),
            ("belak.temperature".to_string(), entry(65.2, 45)),
            ("temperature".to_string(), entry(1.0, 10)),
            ("deploy_test".to_string(), entry(1.0, 10)),
        ]);
        let houses = group_houses(latest);
        assert_eq!(houses.keys().collect::<Vec<_>>(), vec!["belak", "jsvana"]);
        assert_eq!(houses["jsvana"]["temperature"].value, 77.0);
    }

    #[test]
    fn formats_house_line() {
        let metrics = BTreeMap::from_iter([
            ("temperature".to_string(), entry(62.14, 42)),
            ("feels_like".to_string(), entry(62.0, 42)),
            ("humidity".to_string(), entry(85.4, 42)),
            ("wind_speed".to_string(), entry(3.0, 42)),
        ]);
        let line = format_house("jsvana", &metrics, Some((62.1, 79.8)));
        assert_eq!(
            line,
            "jsvana: Currently 62.1°F, Feels Like 62.0°F. High 79.8°F, Low 62.1°F. Humidity 85%."
        );
    }

    #[test]
    fn rain_bands() {
        let cases = [
            (None, None),
            (Some(0.0), None),
            (Some(0.004), Some("Trace rain")),
            (Some(0.01), Some("Light rain, 0.01 in/hr")),
            (Some(0.0999), Some("Light rain, 0.10 in/hr")),
            (Some(0.1), Some("Moderate rain, 0.10 in/hr")),
            (Some(0.3), Some("Heavy rain, 0.30 in/hr")),
            (Some(2.0), Some("Torrential rain, 2.00 in/hr")),
        ];
        for (rate, expected) in cases {
            assert_eq!(format_rain(rate).as_deref(), expected, "rate {rate:?}");
        }
    }

    #[test]
    fn formats_house_line_with_rain() {
        let metrics = BTreeMap::from_iter([
            ("temperature".to_string(), entry(50.1, 17)),
            ("humidity".to_string(), entry(92.0, 17)),
            ("rain_rate".to_string(), entry(0.218822834645669, 17)),
        ]);
        let line = format_house("ghavil", &metrics, None);
        assert_eq!(
            line,
            "ghavil: Currently 50.1°F. Humidity 92%. Moderate rain, 0.22 in/hr."
        );
    }

    #[test]
    fn formats_stale_house_line() {
        let metrics = BTreeMap::from_iter([("temperature".to_string(), entry(70.0, 7200))]);
        let line = format_house("belak", &metrics, None);
        assert_eq!(line, "belak: Currently 70.0°F. Last report 2h ago.");
    }

    #[test]
    fn formats_unknown_metrics_only() {
        let metrics = BTreeMap::from_iter([("battery".to_string(), entry(99.0, 42))]);
        let line = format_house("ghavil", &metrics, None);
        assert_eq!(line, "ghavil: 1 metrics reported.");
    }

    fn houses(names: &[&str]) -> BTreeMap<String, BTreeMap<String, LatestEntry>> {
        names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    BTreeMap::from_iter([("temperature".to_string(), entry(70.0, 30))]),
                )
            })
            .collect()
    }

    #[test]
    fn bare_command_picks_the_requesters_house() {
        let selection = select_houses("", Some("jsvana"), &houses(&["belak", "jsvana"]));
        assert_eq!(selection, Selection::Houses(vec!["jsvana".to_string()]));
    }

    #[test]
    fn bare_command_matches_the_nick_case_insensitively() {
        let selection = select_houses("", Some("JSvana"), &houses(&["belak", "jsvana"]));
        assert_eq!(selection, Selection::Houses(vec!["jsvana".to_string()]));
    }

    #[test]
    fn bare_command_hints_when_the_nick_matches_no_house() {
        let selection = select_houses("", Some("ghavil"), &houses(&["belak", "jsvana"]));
        assert_eq!(
            selection,
            Selection::Hint(
                "no house named ghavil (houses: belak, jsvana). Try !tempest all.".to_string()
            )
        );
    }

    #[test]
    fn bare_command_without_a_user_shows_every_house() {
        let selection = select_houses("", None, &houses(&["belak", "jsvana"]));
        assert_eq!(
            selection,
            Selection::Houses(vec!["belak".to_string(), "jsvana".to_string()])
        );
    }

    #[test]
    fn all_selects_every_house() {
        let selection = select_houses("all", Some("jsvana"), &houses(&["belak", "jsvana"]));
        assert_eq!(
            selection,
            Selection::Houses(vec!["belak".to_string(), "jsvana".to_string()])
        );
    }

    #[test]
    fn a_named_house_selects_only_that_house() {
        let selection = select_houses("belak", Some("jsvana"), &houses(&["belak", "jsvana"]));
        assert_eq!(selection, Selection::Houses(vec!["belak".to_string()]));
    }

    #[test]
    fn a_named_house_is_matched_case_insensitively_and_trimmed() {
        let selection = select_houses("  BELAK  ", None, &houses(&["belak", "jsvana"]));
        assert_eq!(selection, Selection::Houses(vec!["belak".to_string()]));
    }

    #[test]
    fn an_unknown_house_hints() {
        let selection = select_houses("nope", Some("jsvana"), &houses(&["belak", "jsvana"]));
        assert_eq!(
            selection,
            Selection::Hint(
                "no house named nope (houses: belak, jsvana). Try !tempest all.".to_string()
            )
        );
    }

    #[test]
    fn age_formatting() {
        assert_eq!(format_age(45), "45s ago");
        assert_eq!(format_age(300), "5m ago");
        assert_eq!(format_age(7200), "2h ago");
        assert_eq!(format_age(200_000), "2d ago");
    }
}
