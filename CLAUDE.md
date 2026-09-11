# CLAUDE.md

## Build & Test

```bash
cargo build            # build (needs protoc: brew install protobuf)
cargo test             # unit tests
cargo clippy --all-targets   # lint
cargo fmt              # format
```

Quality gates before committing: `cargo fmt` -> `cargo clippy --all-targets` -> `cargo test`.

## What This Is

A seabird-core bot (Rust, `seabird` crate) exposing one chat command:
`tempest`, which fetches `/api/v1/latest` from a tempest-aggregator instance
and prints one line of weather per house. Runs as a native binary + systemd
unit on blackpearl (role `seabird_tempest` in ansible-blackpearl). Pushes to
main are auto-deployed: the build-main workflow publishes a rolling `latest`
prerelease, and a `seabird-tempest-autodeploy.timer` on blackpearl polls it
every 5 minutes.

## Architecture

Single file, `src/main.rs`:

- `main` — env config, reconnect loop with backoff + 10-minute idle watchdog
  (mirrors seabird-ham), registers the `tempest` command via `stream_events`.
- `handle_tempest` — fetch latest, group, pick houses via `select_houses`, then
  per-house 24h temperature range (distribution endpoint) and one
  `send_message` per house.
- `select_houses` — pure arg/nick routing: bare command picks the requester's
  house by nick, `all` picks every house, anything else is a house name; no
  match yields a hint listing known houses. Matching is case-insensitive.
- `group_houses` — splits `<house>.<metric>` names; unprefixed metrics are
  ignored (legacy data, test junk).
- `format_house` / `format_rain` / `format_wind` / `compass` / `format_age` —
  pure formatting, unit-tested. Lines are `<house>: Currently ...°F, Feels Like
  ... High/Low ... Humidity ...` with a rain clause only when `rain_rate` is
  nonzero, a wind clause only when there is wind, and a stale note past 15
  minutes.

Config is env-only: `SEABIRD_URL`, `SEABIRD_TOKEN`, `TEMPEST_URL`,
`TEMPEST_TOKEN`. Values are assumed imperial.

## Code Standards

- Rust 2021 edition, rustfmt defaults, clippy clean.
- Keep formatting functions pure so they stay unit-testable.
