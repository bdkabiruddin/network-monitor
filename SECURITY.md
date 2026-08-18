# Security Policy

## Supported Versions

Only the latest [release](https://github.com/bdkabiruddin/network-monitor/releases)
is supported. Please upgrade before reporting an issue.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Instead, email **bd.kabiruddin@gmail.com** with:
- A description of the vulnerability and its potential impact
- Steps to reproduce it
- Any relevant logs (e.g. `journalctl --user -b 0 | grep -i network-monitor`)

You should get a response within a few days. Once a fix is available, it
will be released and the report credited (unless you'd prefer otherwise).

## Scope notes

This is a local desktop utility: it reads `/proc/net/dev`, `/proc/net/route`
and `/sys/class/net`, writes to a local SQLite database under
`~/.local/share/network-monitor/`, and (Tauri build only) talks to the
session D-Bus for the system tray icon. It does make outbound network
requests of its own -- a single ICMP echo (`ping`) to 1.1.1.1 every few
seconds for latency measurement, and nothing else -- and does not run with
elevated privileges except for the optional, explicitly user-triggered
NetHogs/Firewall/Packet Inspector scans (via `pkexec`, prompting for a
password each time). It does not accept remote input. Reports involving
local D-Bus exposure, `/proc`/`/sys` parsing, `ss`/`nethogs`/`nft` output
parsing, SQLite handling, or any `pkexec`-gated scan are all in scope.
