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
and prints one line of weather per house. Runs in a container on blackpearl
(role `seabird_tempest` in ansible-blackpearl); image at
`ghcr.io/jsvana/seabird-tempest`.

## Architecture

Single file, `src/main.rs`:

- `main` — env config, reconnect loop with backoff + 10-minute idle watchdog
  (mirrors seabird-ham), registers the `tempest` command via `stream_events`.
- `handle_tempest` — fetch latest + per-house 24h temperature range
  (distribution endpoint), group, one `send_message` per house.
- `group_houses` — splits `<house>.<metric>` names; unprefixed metrics are
  ignored (legacy data, test junk).
- `format_house` / `format_age` — pure formatting, unit-tested. Lines are
  `<house>: Currently ...°F, Feels Like ... High/Low ... Humidity ...` with a
  stale note past 15 minutes.

Config is env-only: `SEABIRD_URL`, `SEABIRD_TOKEN`, `TEMPEST_URL`,
`TEMPEST_TOKEN`. Values are assumed imperial.

## Code Standards

- Rust 2021 edition, rustfmt defaults, clippy clean.
- Keep formatting functions pure so they stay unit-testable.
