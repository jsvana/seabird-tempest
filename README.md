# seabird-tempest

A [seabird](https://github.com/seabird-chat) bot with one job: the `tempest`
command prints the latest weather from a
[tempest-aggregator](https://github.com/jsvana/tempest-aggregator) instance.

With no argument it prints the house matching your nick:

```
<jsvana> !tempest
<seabird> jsvana: Currently 62.1°F, Feels Like 62.0°F. High 79.8°F, Low 62.1°F. Humidity 85%.
```

`!tempest <house>` prints someone else's, and `!tempest all` prints every house.
Nick and house matching is case-insensitive. If nothing matches, the bot says so
and lists the houses it knows about:

```
<ghavil> !tempest
<seabird> ghavil: no house named ghavil (houses: belak, jsvana). Try !tempest all.
```

## The house convention

The aggregator has a flat metric namespace, so houses are encoded in metric
names: `<house>.<metric>` (e.g. `jsvana.temperature`, `belak.wind_speed`).
Anyone with a write token pushes their house's numbers:

```bash
curl -X POST -H "Authorization: Bearer twa_..." -H 'Content-Type: application/json' \
  -d '[{"metric": "belak.temperature", "value": 65.2},
       {"metric": "belak.humidity", "value": 71}]' \
  https://tempest.westpeninsulashould.works/api/v1/observations
```

The bot groups `/api/v1/latest` by the prefix and prints one line per selected
house, always prefixed `<house>: `. Metrics without a `.` prefix are ignored. Values
are assumed imperial (°F, in/hr). The line shows `temperature` (with `feels_like`),
the 24h high/low (from the aggregator's distribution endpoint), and
`humidity`; other metrics are stored and queryable but not shown. Houses
whose freshest sample is older than 15 minutes get a "Last report X ago"
note.

A nonzero `rain_rate` adds a rain clause after the humidity, naming the
intensity and the rate; nothing is printed when it isn't raining:

```
<ghavil> !tempest
<seabird> ghavil: Currently 50.1°F, Feels Like 48.0°F. High 62.0°F, Low 49.0°F. Humidity 92%. Moderate rain, 0.22 in/hr.
```

Intensity uses the standard rain-rate bands converted to in/hr: light below
0.1, moderate below 0.3, heavy below 2.0, torrential above. Rates under 0.01
in/hr print as `Trace rain` without a number, which is also where the haptic
sensor's stray taps land.

## Configuration (environment)

| Variable | Default | Description |
|----------|---------|-------------|
| `SEABIRD_URL` | `https://api.seabird.chat` | seabird-core gRPC endpoint |
| `SEABIRD_TOKEN` | required | seabird-core auth token |
| `TEMPEST_URL` | required | tempest-aggregator base URL |
| `TEMPEST_TOKEN` | required | aggregator token with `read` scope |

## Docker

```bash
docker build -t seabird-tempest .
docker run -d -e SEABIRD_TOKEN=... -e TEMPEST_URL=https://tempest.westpeninsulashould.works -e TEMPEST_TOKEN=twa_... seabird-tempest
```

Images are published to `ghcr.io/jsvana/seabird-tempest` on pushes to main.
