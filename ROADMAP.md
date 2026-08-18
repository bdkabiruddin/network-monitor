# Network Monitor — feature status

A Tauri 2 desktop app: a Rust backend (`app/`, backed by the `netmon-core`
library at `core/`) driving a plain HTML/CSS/JS frontend (`frontend/`, no
bundler or build step).

## Architecture

- `core/` — `netmon-core`, a data-layer library with no UI code:
  `/proc/net/dev`, `/sys/class/net/*`, `ip route`, `ping`, `ss -tuinp`,
  `nmcli`, `nethogs`/`nft`/`tcpdump` (via `pkexec`), and a SQLite history
  store.
- `app/` — Tauri commands and app state. A background thread polls
  bandwidth/connections/history/anomaly-detection every 2s and latency
  every 5s into shared state; everything else is a synchronous on-demand
  command, fetched by the frontend while its screen is visible.
- `frontend/` — static HTML/CSS/JS driving the above via Tauri's
  `invoke()`.

Single-instance enforced via `tauri-plugin-single-instance`; a second
launch attempt focuses the existing window instead of starting a second
process.

## Screens

- **Dashboard** — live download/upload rate, MAC/MTU/link speed/state,
  latency, totals, active connections, errors/drops, peak stats, a 24h
  traffic chart and a live telemetry chart (both with hover tooltips and
  a range selector), and a manual NetHogs scan (`pkexec`) for top
  bandwidth consumers.
- **Connections** — live TCP/UDP socket table (`ss -tuinp`) with real
  per-connection sent/recv byte counters and rates, sortable/paginated.
  Includes a Geo-IP map (DB-IP's free City + ASN databases, downloaded
  on demand, not bundled) with a paginated peer table showing city,
  country, and registrant.
- **Processes** — processes ranked by open connection count, plus a
  manual NetHogs scan for real per-process bandwidth.
- **Wi-Fi Analyzer** — connected network details and a searchable
  nearby-networks table via `nmcli`.
- **Interfaces** — every interface the kernel knows about, bonding/
  failover groups, and LAN/ARP devices.
- **VPN** — NetworkManager VPN profiles with connect/disconnect.
- **Reports** — generates a CSV export of recorded history for a
  selected period; delete/delete-all with a confirm step.
- **Speed Test** — a real transfer-based speed test, ping/traceroute,
  DNS resolver stats, a one-shot packet capture (`pkexec tcpdump`), and
  a plain TCP port scan.
- **Alerts** — a rolling-baseline anomaly detector on the bandwidth
  stream, with optional desktop notifications.
- **Firewall** — read-only nftables ruleset viewer (`pkexec nft`),
  grouped by table/chain. Rule mutation is intentionally not
  implemented.
- **Settings** — autostart, refresh interval, notification toggles, a
  data usage cap tracked against real monthly transfer totals, history
  retention with automatic pruning, and history management (clear all,
  live row count).

## Known gaps / deliberately deferred

- Firewall rule mutation (add/remove/toggle) — reading is real; writing
  is out of scope given the risk of a bad rule cutting off connectivity.
- QoS / scheduled bandwidth throttling — would need real `tc` qdisc/class
  manipulation plus a scheduler; same mutation-risk caution as firewall
  rules.
- No i18n — the language setting persists but doesn't translate anything.
- Multi-user profiles — this is single-user desktop software; the
  Settings profile shows the actual logged-in OS user.
- Scheduled email reports — would need SMTP configuration this app
  doesn't collect.
- "Check for updates" — disabled pending a real Tauri-build release
  under this project's own GitHub Releases.
