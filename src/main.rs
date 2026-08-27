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

async fn fetch_latest(config: &TempestConfig) -> Result<HashMap<String, LatestEntry>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
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

fn compass(degrees: f64) -> &'static str {
    const DIRECTIONS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let index = ((degrees.rem_euclid(360.0) / 22.5).round() as usize) % 16;
    DIRECTIONS[index]
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

fn format_house(house: &str, metrics: &BTreeMap<String, LatestEntry>) -> String {
    let value = |name: &str| metrics.get(name).map(|entry| entry.value);
    let mut parts = Vec::new();

    if let Some(temperature) = value("temperature") {
        let mut part = format!("🌡️ {temperature:.1}°F");
        if let Some(feels_like) = value("feels_like") {
            if (feels_like - temperature).abs() >= 1.0 {
                part.push_str(&format!(" (feels {feels_like:.1})"));
            }
        }
        parts.push(part);
    }
    if let Some(humidity) = value("humidity") {
        parts.push(format!("💧 {humidity:.0}%"));
    }
    if let Some(wind) = value("wind_speed_average").or_else(|| value("wind_speed")) {
        let mut part = format!("🌬️ {wind:.1} mph");
        let mut extras = Vec::new();
        if let Some(gust) = value("wind_gust") {
            extras.push(format!("gust {gust:.1}"));
        }
        if let Some(direction) = value("wind_direction") {
            extras.push(compass(direction).to_string());
        }
        if !extras.is_empty() {
            part.push_str(&format!(" ({})", extras.join(", ")));
        }
        parts.push(part);
    }
    if let Some(rain_rate) = value("rain_rate") {
        if rain_rate > 0.0 {
            parts.push(format!("☔ {rain_rate:.2} in/h"));
        }
    }
    if let Some(uv_index) = value("uv_index") {
        parts.push(format!("☀️ UV {uv_index:.1}"));
    }
    if let Some(pressure) = value("pressure") {
        parts.push(format!("{pressure:.2} inHg"));
    }

    if parts.is_empty() {
        // A house pushing only metrics this bot doesn't format still shows up.
        parts.push(format!("{} metrics", metrics.len()));
    }

    let age = metrics
        .values()
        .map(|entry| entry.age_secs)
        .min()
        .unwrap_or(0);
    format!("{house}: {} — {}", parts.join(" "), format_age(age))
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
) -> Result<()> {
    let houses = match fetch_latest(tempest).await {
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

    for (house, metrics) in &houses {
        client
            .send_message(
                command_source.channel_id.clone(),
                format_house(house, metrics),
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
            handle_tempest(client, tempest, command_source).await?;
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
            short_help: "latest weather at all houses".to_string(),
            full_help: "Print the latest Tempest weather at every house reporting to the \
                        aggregator."
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
            ("temperature".to_string(), entry(77.03, 42)),
            ("humidity".to_string(), entry(66.4, 42)),
            ("wind_speed_average".to_string(), entry(3.02, 42)),
            ("wind_gust".to_string(), entry(5.2, 42)),
            ("wind_direction".to_string(), entry(350.0, 42)),
            ("rain_rate".to_string(), entry(0.0, 42)),
            ("uv_index".to_string(), entry(2.51, 42)),
            ("pressure".to_string(), entry(29.93, 42)),
        ]);
        let line = format_house("jsvana", &metrics);
        assert_eq!(
            line,
            "jsvana: 🌡️ 77.0°F 💧 66% 🌬️ 3.0 mph (gust 5.2, N) ☀️ UV 2.5 29.93 inHg — 42s ago"
        );
    }

    #[test]
    fn compass_points() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(350.0), "N");
        assert_eq!(compass(347.0), "NNW");
        assert_eq!(compass(90.0), "E");
        assert_eq!(compass(200.0), "SSW");
    }

    #[test]
    fn age_formatting() {
        assert_eq!(format_age(45), "45s ago");
        assert_eq!(format_age(300), "5m ago");
        assert_eq!(format_age(7200), "2h ago");
        assert_eq!(format_age(200_000), "2d ago");
    }
}
