# seabird-tempest

A [seabird](https://github.com/seabird-chat) bot with one job: the `tempest`
command prints the latest weather at every house reporting to a
[tempest-aggregator](https://github.com/jsvana/tempest-aggregator) instance.

```
<jsvana> !tempest
<seabird> jsvana: 🌡️ 77.0°F 💧 66% 🌬️ 3.0 mph (gust 5.2, N) ☀️ UV 2.5 29.93 inHg — 42s ago
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

The bot groups `/api/v1/latest` by the prefix and prints one line per house.
Metrics without a `.` prefix are ignored. Values are assumed imperial
(°F, mph, in/h, inHg). Recognized metric names: `temperature`, `feels_like`,
`humidity`, `wind_speed`/`wind_speed_average`, `wind_gust`, `wind_direction`
(degrees), `rain_rate`, `uv_index`, `pressure`.

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
