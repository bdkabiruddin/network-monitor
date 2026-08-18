# Network Monitor

[![CI](https://github.com/bdkabiruddin/network-monitor/actions/workflows/ci.yml/badge.svg)](https://github.com/bdkabiruddin/network-monitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A live network dashboard for Linux: download/upload bandwidth, latency,
active TCP/UDP connections, per-process network activity, Wi-Fi analysis,
firewall/VPN/interfaces detail, and long-term traffic history, backed by a
local SQLite database (`~/.local/share/network-monitor/network_history.db`).

This repo is a Tauri 2 native shell around a real HTML/CSS/JS frontend (no
bundler) — see **[`ROADMAP.md`](ROADMAP.md)** for what's implemented per
screen and known gaps.

The UI is backed by **[`core/`](core)** (`netmon-core`), a data-layer
library with no UI code of its own: `/proc/net/dev`, `/sys/class/net/*`,
`ip route`, `ping`, `ss -tunp`, `nmcli`, `nethogs` (via `pkexec`), and the
SQLite history store. Nothing in the app fabricates data — a feature
either shows a real reading or an honest "—"/"not implemented"
placeholder.

## Building

```sh
git clone https://github.com/bdkabiruddin/network-monitor
cd network-monitor

# Needs the Tauri Linux prerequisites: webkit2gtk-4.1-dev,
# libayatana-appindicator3-dev, librsvg2-dev, build-essential, libssl-dev
cd app && cargo run
```

## Project structure

```
core/                  netmon-core: the data layer (no UI)
app/                    Tauri 2 Rust backend
frontend/               HTML/CSS/JS frontend (no build step)
.github/               CI workflow, issue/PR templates
SECURITY.md            Vulnerability reporting, privilege/network scope
```

## License

[MIT](LICENSE)
