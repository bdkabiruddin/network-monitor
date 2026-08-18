use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{Datelike, Local};
use netmon_core::alerts::{AnomalyDetector, Severity};
use netmon_core::format::{format_bytes_rate, format_bytes_total, format_duration};
use netmon_core::history::{self, HistoryDb, DEFAULT_RETENTION_DAYS};
use netmon_core::netdev::{self, NetworkReader, Reading};
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

const UPDATE_MS: u64 = 2000;
const LATENCY_INTERVAL_S: u64 = 5;
const ALERTS_KEPT: usize = 200;

/// A panic while holding one of these locks (nowhere currently panics
/// while holding one, but the background poll thread is unsupervised —
/// if that ever changes) would otherwise poison the mutex permanently,
/// making every command touching shared state panic forever until the
/// process restarts. Recovering the guard instead just carries on with
/// whatever state existed at the panic — acceptable for in-memory
/// UI-refresh data that gets overwritten by the next 2s poll tick
/// anyway, and far better than bricking the whole dashboard.
trait LockIgnorePoison<T> {
    fn lock_ip(&self) -> MutexGuard<'_, T>;
}
impl<T> LockIgnorePoison<T> for Mutex<T> {
    fn lock_ip(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Clone)]
struct AlertItem {
    id: u64,
    severity: Severity,
    message: String,
    time: String,
    read: bool,
}

#[derive(Default)]
struct Shared {
    reading: Reading,
    latency_ms: Option<f64>,
    peak_download: f64,
    peak_upload: f64,
    connections_count: usize,
    last_updated: String,
    alerts: Vec<AlertItem>,
    next_alert_id: u64,
    detection_enabled: bool,
}

struct DashboardState {
    shared: Arc<Mutex<Shared>>,
    // Separate connection from the background writer's — WAL mode (set
    // in HistoryDb::open) lets both coexist without lock contention, same
    // pattern as the egui build's `AppState::history_reader`.
    history_reader: Mutex<HistoryDb>,
    // Persists across get_connections_page calls (one per poll tick
    // while Connections is the visible page) so per-connection sent/recv
    // rates and observed duration can be tracked across polls — a rate
    // needs two samples, and this is the only call site that touches it,
    // so one tracker instance here is enough (get_processes fetches its
    // own independent snapshot that doesn't need either).
    connection_tracker: Mutex<netmon_core::connections::ConnectionRateTracker>,
}

/// Holds the live tray icon, if the "Show in system tray" setting is
/// currently on — `None` when it's off. Toggling the setting at
/// runtime creates/destroys this rather than just changing a flag,
/// since Tauri's tray icon is a real OS resource, not a CSS class.
struct TrayState(Mutex<Option<TrayIcon>>);

/// Holds the opened GeoIP database reader (~130MB loaded once), or an
/// inert `None`-backed reader if no database has been downloaded yet.
/// Cached at app-state level rather than reopened per lookup/command
/// call — Connections polls every 2s while visible, and re-reading a
/// 130MB file that often would be the same mistake the 24H chart's
/// earlier fix was about, just with a file read instead of a SQL scan.
struct GeoIpState(Mutex<netmon_core::geoip::GeoIpReader>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardSnapshot {
    default_iface: String,
    iface_state: String,
    download_rate: String,
    upload_rate: String,
    mac: String,
    mtu: String,
    link_speed: String,
    latency: String,
    total_transferred: String,
    total_downloaded: String,
    total_uploaded: String,
    active_connections: usize,
    errors_drops: u64,
    peak_download: String,
    peak_upload: String,
    total_data: String,
    last_updated: String,
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[tauri::command]
fn get_dashboard(state: tauri::State<DashboardState>) -> DashboardSnapshot {
    let s = state.shared.lock_ip();
    let info = &s.reading.primary_info;
    let errors_drops = s
        .reading
        .default_iface
        .as_ref()
        .and_then(|name| s.reading.interfaces.get(name))
        .map(|r| r.counters.rx_errs + r.counters.rx_drop + r.counters.tx_errs + r.counters.tx_drop)
        .unwrap_or(0);

    DashboardSnapshot {
        default_iface: s.reading.default_iface.clone().unwrap_or_else(|| "—".to_string()),
        iface_state: if info.operstate.is_empty() || info.operstate == "unknown" {
            "—".to_string()
        } else {
            capitalize(&info.operstate)
        },
        download_rate: format_bytes_rate(Some(s.reading.download_rate)),
        upload_rate: format_bytes_rate(Some(s.reading.upload_rate)),
        mac: info.address.clone().unwrap_or_else(|| "—".to_string()),
        mtu: info.mtu.clone().unwrap_or_else(|| "—".to_string()),
        link_speed: info.speed_mbps.map(|m| format!("{m} Mbit/s")).unwrap_or_else(|| "—".to_string()),
        latency: s.latency_ms.map(|v| format!("{v:.0} ms")).unwrap_or_else(|| "— ms".to_string()),
        total_transferred: format_bytes_total(Some(s.reading.total_downloaded + s.reading.total_uploaded)),
        total_downloaded: format_bytes_total(Some(s.reading.total_downloaded)),
        total_uploaded: format_bytes_total(Some(s.reading.total_uploaded)),
        active_connections: s.connections_count,
        errors_drops,
        peak_download: format_bytes_rate(Some(s.peak_download)),
        peak_upload: format_bytes_rate(Some(s.peak_upload)),
        total_data: format_bytes_total(Some(s.reading.total_downloaded + s.reading.total_uploaded)),
        last_updated: s.last_updated.clone(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChartData {
    download: Vec<f64>,
    upload: Vec<f64>,
    latency: Vec<f64>,
}

#[tauri::command]
fn get_traffic_chart(bin_seconds: i64, state: tauri::State<DashboardState>) -> ChartData {
    let bins = ((24 * 3600) / bin_seconds.max(1)).max(1);
    let db = state.history_reader.lock_ip();
    let split = db.interval_totals_split(24, bins);
    let latency = db.interval_latency_avg(24, bins);
    ChartData {
        download: split.iter().map(|(dl, _)| *dl).collect(),
        upload: split.iter().map(|(_, ul)| *ul).collect(),
        latency: latency.iter().map(|v| v.unwrap_or(0.0)).collect(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveMetricsChart {
    download: Vec<f64>,
    upload: Vec<f64>,
    latency: Vec<f64>,
    timestamps: Vec<String>,
}

/// Powers the Live Metrics card's "Last N min" selector and the Live
/// Traffic screen's chart — raw (2s-poll resolution, unbucketed for
/// windows up to an hour) or auto-bucketed rows from the same history
/// db the 24H chart reads, rather than the short in-memory ring buffer
/// the old fixed sparkline used. Real per-row timestamps come back
/// too, since samples aren't evenly spaced (a missed poll, or a
/// bucket's actual width) — better than the caller assuming a fixed
/// interval between points.
#[tauri::command]
fn get_live_metrics_chart(minutes: i64, state: tauri::State<DashboardState>) -> LiveMetricsChart {
    let seconds = minutes.max(1) * 60;
    let db = state.history_reader.lock_ip();
    let rows = db.graph_data(seconds, None, 120);
    LiveMetricsChart {
        download: rows.iter().map(|r| r.download_rate.unwrap_or(0.0)).collect(),
        upload: rows.iter().map(|r| r.upload_rate.unwrap_or(0.0)).collect(),
        latency: rows.iter().map(|r| r.latency_ms.unwrap_or(0.0)).collect(),
        timestamps: rows.into_iter().map(|r| r.timestamp).collect(),
    }
}

// — Connections screen —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionRow {
    proto: String,
    local: String,
    peer: String,
    process: String,
    state: String,
    // Real cumulative totals from ss -i's TCP_INFO (TCP only — UDP has
    // no such counters, so these are "—" for UDP rows). Rates need a
    // second sample before they're meaningful, so a connection's very
    // first tick after appearing shows "—" for sent/recvRate too — that
    // gap is honest, not a bug to paper over with an assumed value.
    sent: String,
    recv: String,
    sent_rate: String,
    recv_rate: String,
    // Real, but honestly scoped: time since *this app* first observed
    // the connection, not the socket's true wall-clock age (ss exposes
    // no such field at all — see ConnectionRateTracker's doc comment).
    // Every connection has one from its very first sighting, unlike
    // sent/recvRate which need a second poll.
    duration: String,
    // Raw values alongside the formatted display strings so the
    // frontend can sort numerically instead of lexicographically on
    // "1.6 MB" — null (not 0) when genuinely unavailable, so a sort
    // doesn't quietly treat "no data" as "zero".
    sent_bytes: Option<u64>,
    recv_bytes: Option<u64>,
    sent_bps: Option<f64>,
    recv_bps: Option<f64>,
    duration_secs: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoIpStatusOut {
    present: bool,
    age_days: Option<i64>,
}

#[tauri::command]
fn get_geoip_status() -> GeoIpStatusOut {
    let s = netmon_core::geoip::database_status();
    GeoIpStatusOut { present: s.present, age_days: s.age_days }
}

/// A real, on-demand download (~65MB) — like the pkexec-gated scans,
/// deliberately not something that runs automatically on a timer.
#[tauri::command]
fn download_geoip_database(state: tauri::State<GeoIpState>) -> Result<(), String> {
    netmon_core::geoip::download_database()?;
    *state.0.lock_ip() = netmon_core::geoip::GeoIpReader::open();
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoConnectionRow {
    ip: String,
    city: String,
    country: String,
    process: String,
    lat: f64,
    lon: f64,
    // Real ASN + org name ("AS15169 · Google LLC") from the separate,
    // much smaller ASN database — downloaded alongside the city one but
    // independently optional, so this reads "—" rather than blocking
    // the rest of the row when it's missing (not downloaded yet, or the
    // ASN database genuinely has no entry for this address).
    registrant: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionsPage {
    connections: Vec<ConnectionRow>,
    geo: Vec<GeoConnectionRow>,
}

/// The Connections page's main table and its Geo-IP card both need a
/// fresh `ss -tuinp` snapshot every time the page is visible and polls —
/// originally two separate commands (get_connections, get_connections_geo)
/// each called their own `read_connections`, meaning two real `ss`
/// subprocess spawns (~80ms each on this machine) plus a third from the
/// always-on background poll thread's connection count, all within the
/// same ~instant. Merged into one command sharing a single snapshot —
/// same "don't repeat expensive work per tick" reasoning as the 24H
/// chart's and GeoIP reader's earlier fixes, just for a subprocess call
/// instead of a SQL scan or a file read.
#[tauri::command]
fn get_connections_page(dash: tauri::State<DashboardState>, geo: tauri::State<GeoIpState>) -> ConnectionsPage {
    let conns = netmon_core::connections::read_connections(500);
    let tracks = dash.connection_tracker.lock_ip().update(&conns);

    let connections = conns
        .iter()
        .map(|c| {
            let track = tracks.get(&netmon_core::connections::ConnectionRateTracker::key(c));
            let rate = track.and_then(|t| t.rate);
            ConnectionRow {
                proto: c.proto.to_uppercase(),
                local: c.local.clone(),
                peer: c.peer.clone(),
                process: if c.proc_name.is_empty() { "—".to_string() } else { format!("{} ({})", c.proc_name, c.pid) },
                state: c.state.clone(),
                sent: c.sent_bytes.map(|b| format_bytes_total(Some(b as f64))).unwrap_or_else(|| "—".to_string()),
                recv: c.recv_bytes.map(|b| format_bytes_total(Some(b as f64))).unwrap_or_else(|| "—".to_string()),
                sent_rate: rate.map(|r| format_bytes_rate(Some(r.sent_bps))).unwrap_or_else(|| "—".to_string()),
                recv_rate: rate.map(|r| format_bytes_rate(Some(r.recv_bps))).unwrap_or_else(|| "—".to_string()),
                duration: format_duration(track.map(|t| t.duration_secs)),
                sent_bytes: c.sent_bytes,
                recv_bytes: c.recv_bytes,
                sent_bps: rate.map(|r| r.sent_bps),
                recv_bps: rate.map(|r| r.recv_bps),
                duration_secs: track.map(|t| t.duration_secs).unwrap_or(0.0),
            }
        })
        .collect();

    let reader = geo.0.lock_ip();
    let mut seen = std::collections::HashSet::new();
    let mut geo_rows = Vec::new();
    for c in &conns {
        let ip = netmon_core::connections::peer_ip(&c.peer);
        if ip.is_empty() || !seen.insert(ip.to_string()) {
            continue;
        }
        let Some(loc) = reader.lookup(ip) else { continue };
        let registrant = match (loc.asn, &loc.asn_org) {
            (Some(asn), Some(org)) => format!("AS{asn} · {org}"),
            (None, Some(org)) => org.clone(),
            (Some(asn), None) => format!("AS{asn}"),
            (None, None) => "—".to_string(),
        };
        geo_rows.push(GeoConnectionRow {
            ip: ip.to_string(),
            city: loc.city.unwrap_or_else(|| "—".to_string()),
            country: loc.country.unwrap_or_else(|| "—".to_string()),
            process: if c.proc_name.is_empty() { "—".to_string() } else { c.proc_name.clone() },
            lat: loc.lat,
            lon: loc.lon,
            registrant,
        });
    }

    ConnectionsPage { connections, geo: geo_rows }
}

// — Processes screen —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessRow {
    pid: String,
    name: String,
    connections: usize,
}

/// Ranked by open connection count, not bandwidth — see the comment on
/// `netmon_core::connections::top_processes_from`: real per-process
/// bandwidth needs nethogs running as root, not wired up here.
#[tauri::command]
fn get_processes() -> Vec<ProcessRow> {
    netmon_core::connections::read_top_processes_by_connections(50)
        .into_iter()
        .map(|(pid, name, connections)| ProcessRow { pid, name, connections })
        .collect()
}

// — Wi-Fi Analyzer screen —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WifiNetworkRow {
    in_use: bool,
    ssid: String,
    security: String,
    channel: String,
    band: String,
    speed: String,
    bssid: String,
    signal: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WifiSnapshot {
    connected: Option<WifiNetworkRow>,
    networks: Vec<WifiNetworkRow>,
}

fn wifi_row(n: &netmon_core::wifi::WifiNetwork) -> WifiNetworkRow {
    WifiNetworkRow {
        in_use: n.in_use,
        ssid: if n.ssid.is_empty() { "—".to_string() } else { n.ssid.clone() },
        security: if n.security.is_empty() { "Open".to_string() } else { n.security.clone() },
        channel: n.channel.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string()),
        band: n
            .freq_mhz
            .map(|f| if f < 3000 { "2.4 GHz".to_string() } else { "5 GHz".to_string() })
            .unwrap_or_else(|| "—".to_string()),
        speed: n.rate_mbps.map(|r| format!("{r} Mbit/s")).unwrap_or_else(|| "—".to_string()),
        bssid: n.bssid.clone(),
        signal: n.signal_pct.unwrap_or(0),
    }
}

#[tauri::command]
fn get_wifi() -> WifiSnapshot {
    let networks = netmon_core::wifi::read_wifi_networks(false, Duration::from_secs(5));
    let connected = networks.iter().find(|n| n.in_use).map(wifi_row);
    WifiSnapshot { connected, networks: networks.iter().map(wifi_row).collect() }
}

// — Interfaces screen —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InterfaceRow {
    name: String,
    kind: String,
    ip: String,
    mac: String,
    status: String,
    speed: String,
    rx: String,
    tx: String,
    is_default: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BondRow {
    name: String,
    mode: String,
    active: String,
    standby: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LanRow {
    ip: String,
    mac: String,
    iface: String,
    state: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InterfacesSnapshot {
    interfaces: Vec<InterfaceRow>,
    bonding: Vec<BondRow>,
    lan: Vec<LanRow>,
}

#[tauri::command]
fn get_interfaces() -> InterfacesSnapshot {
    let interfaces = netmon_core::interfaces::list_all_interfaces()
        .into_iter()
        .map(|i| InterfaceRow {
            name: i.name,
            kind: i.kind.to_string(),
            ip: i.ipv4.unwrap_or_else(|| "—".to_string()),
            mac: i.mac.unwrap_or_else(|| "—".to_string()),
            status: capitalize(&i.operstate),
            speed: i.speed_mbps.map(|s| format!("{s} Mbit/s")).unwrap_or_else(|| "—".to_string()),
            rx: format_bytes_total(Some(i.rx_bytes as f64)),
            tx: format_bytes_total(Some(i.tx_bytes as f64)),
            is_default: i.is_default,
        })
        .collect();

    let bonding = netmon_core::interfaces::list_bond_groups()
        .into_iter()
        .map(|b| {
            let standby: Vec<String> =
                b.slaves.iter().filter(|s| Some(s.as_str()) != b.active_slave.as_deref()).cloned().collect();
            BondRow {
                name: b.name,
                mode: b.mode,
                active: b.active_slave.unwrap_or_else(|| "—".to_string()),
                standby: if standby.is_empty() { "—".to_string() } else { standby.join(", ") },
            }
        })
        .collect();

    let lan = netmon_core::lan::list_lan_devices()
        .into_iter()
        .map(|d| LanRow {
            ip: d.ip,
            mac: d.mac.unwrap_or_else(|| "—".to_string()),
            iface: d.iface.unwrap_or_else(|| "—".to_string()),
            state: d.state,
        })
        .collect();

    InterfacesSnapshot { interfaces, bonding, lan }
}

// — VPN screen —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VpnProfileRow {
    name: String,
    vpn_type: String,
    active: bool,
}

#[tauri::command]
fn get_vpn() -> Vec<VpnProfileRow> {
    netmon_core::vpn::list_vpn_profiles()
        .into_iter()
        .map(|p| VpnProfileRow { name: p.name, vpn_type: p.vpn_type, active: p.active })
        .collect()
}

// — Reports screen —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportRow {
    name: String,
    period: String,
    date: String,
    size: String,
    // Raw values alongside the pretty-printed ones above -- needed for
    // real sorting (can't numerically/chronologically sort "2.3 MB" or
    // "Aug 18, 22:06" strings) and for remove_report, which needs the
    // real path, not just a display name.
    size_bytes: u64,
    modified_ts: i64,
    path: String,
}

fn report_period_label(name: &str) -> &'static str {
    if name.contains("-24h-") {
        "Last 24 hours"
    } else if name.contains("-7d-") {
        "Last 7 days"
    } else if name.contains("-30d-") {
        "Last 30 days"
    } else {
        "—"
    }
}

#[tauri::command]
fn get_reports() -> Vec<ReportRow> {
    netmon_core::reports::list_reports()
        .into_iter()
        .map(|r| ReportRow {
            period: report_period_label(&r.name).to_string(),
            date: r.modified.map(|d| d.format("%b %-d, %H:%M").to_string()).unwrap_or_else(|| "—".to_string()),
            size: format_bytes_total(Some(r.size_bytes as f64)),
            size_bytes: r.size_bytes,
            modified_ts: r.modified.map(|d| d.timestamp()).unwrap_or(0),
            path: r.path.to_string_lossy().to_string(),
            name: r.name,
        })
        .collect()
}

#[tauri::command]
fn generate_report(period: String, state: tauri::State<DashboardState>) -> Result<String, String> {
    let period = match period.as_str() {
        "24h" => netmon_core::reports::ReportPeriod::Last24h,
        "7d" => netmon_core::reports::ReportPeriod::Last7d,
        "30d" => netmon_core::reports::ReportPeriod::Last30d,
        other => return Err(format!("unknown report period: {other}")),
    };
    let db = state.history_reader.lock_ip();
    netmon_core::reports::generate_csv_report(&db, period).map(|p| p.to_string_lossy().to_string())
}

/// Never wired up until now -- netmon_core::reports::delete_report
/// existed but no Tauri command exposed it, so Reports had no way to
/// remove a generated CSV short of deleting the file by hand. The
/// frontend is responsible for confirming with the user first.
#[tauri::command]
fn remove_report(path: String) -> Result<(), String> {
    netmon_core::reports::delete_report(std::path::Path::new(&path))
}

// — Speed Test screen (DNS Resolver card only) —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DnsSnapshot {
    total_queries: u64,
    cache_size: u64,
    cache_hit_rate: String,
}

#[tauri::command]
fn get_dns() -> Option<DnsSnapshot> {
    netmon_core::dns::read_dns_stats().map(|s| DnsSnapshot {
        total_queries: s.total_transactions,
        cache_size: s.cache_size,
        cache_hit_rate: s.cache_hit_rate_pct().map(|p| format!("{p:.0}%")).unwrap_or_else(|| "—".to_string()),
    })
}

/// Real `ping -c 4`/`traceroute` output, captured whole (not streamed
/// line-by-line — both commands finish in well under Tauri's default
/// command timeout, and a `<pre>` block doesn't need incremental
/// updates the way a live capture would).
#[tauri::command]
fn run_ping(host: String) -> Result<String, String> {
    let output = Command::new("ping").args(["-c", "4", "-W", "2", &host]).output().map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(text)
}

#[tauri::command]
fn run_traceroute(host: String) -> Result<String, String> {
    let output = Command::new("traceroute").args(["-w", "2", "-q", "1", &host]).output().map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(text)
}

// — Packet Inspector (Speed Test screen) — real `tcpdump` output over
// all interfaces, parsed best-effort from its default one-line-per-
// packet text format. Needs root (pkexec), same on-demand-scan pattern
// as Firewall/NetHogs below rather than continuous background capture.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PacketRow {
    time: String,
    source: String,
    destination: String,
    proto: String,
    length: String,
    info: String,
}

const PACKET_CAPTURE_COUNT: u32 = 40;

/// Parses one line of `tcpdump -n -tttt` output, e.g.:
/// `2026-08-17 07:31:00.123456 IP 192.168.0.44.443 > 1.2.3.4.55111: Flags [P.], seq 1:100, ack 1, win 502, length 99`
/// Best-effort: any field this can't confidently pull out of the real
/// line stays "—" rather than a guess — tcpdump's output varies a lot
/// by protocol and this isn't a full parser.
fn parse_tcpdump_line(line: &str) -> Option<PacketRow> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // First two whitespace-separated tokens are "date time.frac" with -tttt.
    let mut parts = line.splitn(3, ' ');
    let date = parts.next()?;
    let time_frac = parts.next()?;
    let rest = parts.next().unwrap_or("");
    let time = format!("{date} {}", time_frac.split('.').next().unwrap_or(time_frac));

    let (before_colon, after_colon) = rest.split_once(':').unwrap_or((rest, ""));
    // before_colon looks like "IP 192.168.0.44.443 > 1.2.3.4.55111" (or "IP6 ...").
    let mut bc = before_colon.split_whitespace();
    let proto_family = bc.next().unwrap_or("");
    let src = bc.next().unwrap_or("—").to_string();
    let _arrow = bc.next();
    let dst = bc.next().unwrap_or("—").to_string();

    let proto = if after_colon.contains("Flags [") {
        "TCP".to_string()
    } else if proto_family == "IP" || proto_family == "IP6" {
        // No TCP flags shown — tcpdump's default UDP/ICMP lines don't
        // name the protocol explicitly either, so this is a best
        // guess from the two most common cases on a normal desktop.
        if after_colon.contains("ICMP") { "ICMP".to_string() } else { "UDP".to_string() }
    } else {
        "—".to_string()
    };

    let length = after_colon
        .rsplit("length ")
        .next()
        .filter(|_| after_colon.contains("length "))
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .map(|s| s.to_string())
        .or_else(|| {
            // UDP/ICMP lines often end in "(N)" for payload length instead.
            after_colon.rsplit('(').next().and_then(|s| s.strip_suffix(')')).map(|s| s.to_string())
        })
        .unwrap_or_else(|| "—".to_string());

    Some(PacketRow { time, source: src, destination: dst, proto, length, info: after_colon.trim().to_string() })
}

#[tauri::command]
fn run_packet_capture() -> Result<Vec<PacketRow>, String> {
    let output = Command::new("pkexec")
        .args(["tcpdump", "-n", "-tttt", "-l", "-i", "any", "-c", &PACKET_CAPTURE_COUNT.to_string()])
        .output()
        .map_err(|e| format!("couldn't start pkexec — is polkit installed? ({e})"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<PacketRow> = stdout.lines().filter_map(parse_tcpdump_line).collect();
    if rows.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().last().unwrap_or("").to_string();
        return Err(format!("capture didn't complete — cancelled, or tcpdump not installed? [{tail}]"));
    }
    Ok(rows)
}

// — Port Scanner (Speed Test screen) — a plain TCP connect scan over a
// fixed list of common ports, no raw sockets/root needed (unlike a SYN
// scan). Scans a host the user names, for their own network's sake —
// same "authorized, defensive, personal-network-monitoring" use this
// whole app is for.

const COMMON_PORTS: &[(u16, &str)] = &[
    (21, "ftp"),
    (22, "ssh"),
    (23, "telnet"),
    (25, "smtp"),
    (53, "domain"),
    (80, "http"),
    (110, "pop3"),
    (139, "netbios-ssn"),
    (143, "imap"),
    (443, "https"),
    (445, "microsoft-ds"),
    (587, "submission"),
    (993, "imaps"),
    (995, "pop3s"),
    (3306, "mysql"),
    (3389, "rdp"),
    (5432, "postgresql"),
    (8080, "http-alt"),
    (8443, "https-alt"),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PortRow {
    port: u16,
    service: String,
    state: String,
}

#[tauri::command]
fn run_port_scan(target: String) -> Result<Vec<PortRow>, String> {
    use std::net::ToSocketAddrs;
    let mut rows = Vec::with_capacity(COMMON_PORTS.len());
    for (port, service) in COMMON_PORTS {
        let addr = format!("{target}:{port}");
        let state = match addr.to_socket_addrs() {
            Ok(mut addrs) => match addrs.next() {
                Some(sockaddr) => match std::net::TcpStream::connect_timeout(&sockaddr, Duration::from_millis(400)) {
                    Ok(_) => "Open",
                    Err(_) => "Closed",
                },
                None => return Err(format!("couldn't resolve {target}")),
            },
            Err(e) => return Err(format!("couldn't resolve {target}: {e}")),
        };
        rows.push(PortRow { port: *port, service: service.to_string(), state: state.to_string() });
    }
    Ok(rows)
}

// — Firewall (read-only) — mirrors the egui build's approach exactly:
// a real `pkexec nft -a list ruleset` scan, grouped by table/chain,
// showing each rule's raw text + handle rather than decomposing
// nftables' match-expression syntax into Name/Direction/Protocol/Port
// columns a half-parse would misrepresent. Mutation (add/remove/toggle)
// is NOT implemented — same "a bad rule can cut off network access"
// caution rust/ROADMAP.md documents; this only ever reads.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FirewallRuleRow {
    text: String,
    handle: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FirewallChainRow {
    table: String,
    name: String,
    hook_info: Option<String>,
    rules: Vec<FirewallRuleRow>,
}

#[tauri::command]
fn scan_firewall() -> Result<Vec<FirewallChainRow>, String> {
    netmon_core::firewall::read_ruleset().map(|chains| {
        chains
            .into_iter()
            .map(|c| FirewallChainRow {
                table: c.table,
                name: c.name,
                hook_info: c.hook_info,
                rules: c.rules.into_iter().map(|r| FirewallRuleRow { text: r.text, handle: r.handle }).collect(),
            })
            .collect()
    })
}

// — Per-process bandwidth (NetHogs, root) — a manual on-demand scan
// (~10s + a password prompt), same as the egui build's ProcMode::Nethogs
// — not continuously polled, unlike the connection-count ranking.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NethogsRowOut {
    pid: Option<String>,
    program: String,
    sent: String,
    received: String,
}

#[tauri::command]
fn scan_nethogs() -> Result<Vec<NethogsRowOut>, String> {
    netmon_core::nethogs::run_nethogs_scan(30).map(|rows| {
        rows.into_iter()
            .map(|r| NethogsRowOut { pid: r.pid, program: r.program, sent: r.sent, received: r.received })
            .collect()
    })
}

// — Speed Test screen's actual test run —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedTestResult {
    ping_ms: Option<f64>,
    download_mbps: Option<f64>,
    upload_mbps: Option<f64>,
    error: Option<String>,
}

/// Real sequential ping → download → upload run against Cloudflare's
/// public endpoint (see netmon_core::speedtest) — takes several seconds,
/// which is fine since this isn't an `async fn` command: Tauri runs
/// plain `fn` commands off the main/event-loop thread already.
#[tauri::command]
fn run_speed_test() -> SpeedTestResult {
    let ping_ms = netdev::measure_latency(netdev::LATENCY_TARGET, 2);
    let download = netmon_core::speedtest::run_download_mbps(netmon_core::speedtest::DEFAULT_DOWNLOAD_TEST_BYTES);
    let upload = netmon_core::speedtest::run_upload_mbps(netmon_core::speedtest::DEFAULT_UPLOAD_TEST_BYTES);
    let error = download.as_ref().err().or(upload.as_ref().err()).cloned();
    SpeedTestResult { ping_ms, download_mbps: download.ok(), upload_mbps: upload.ok(), error }
}

// — VPN connect/disconnect — a normal, reversible NetworkManager
// action (unlike Firewall rule mutation, this can't cut off all network
// access — worst case it drops the VPN tunnel and falls back to the
// underlying connection), so unlike Firewall this is safe to wire up.

#[tauri::command]
fn vpn_connect(name: String) -> Result<(), String> {
    netmon_core::vpn::connect(&name)
}

#[tauri::command]
fn vpn_disconnect(name: String) -> Result<(), String> {
    netmon_core::vpn::disconnect(&name)
}

// — Settings: autostart —

#[tauri::command]
fn get_autostart() -> bool {
    netmon_core::autostart::is_enabled()
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    if enabled {
        netmon_core::autostart::enable()
    } else {
        netmon_core::autostart::disable()
    }
}

/// The real logged-in username, for the sidebar profile card — this is
/// single-user desktop software with no real account system, so "the
/// current OS user" is the only honest identity to show (not a made-up
/// "Local User" placeholder).
#[tauri::command]
fn get_username() -> String {
    std::env::var("USER").unwrap_or_else(|_| "Local User".to_string())
}

// — Settings: everything else, persisted to a small JSON file (not a
// real settings-storage backend, just enough that a preference
// survives a restart). Autostart above is its own source of truth (a
// real XDG file) so it isn't duplicated here.

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    language: String,
    refresh_interval_sec: u64,
    notif_desktop: bool,
    notif_sound: bool,
    notif_tray: bool,
    bandwidth_cap_enabled: bool,
    bandwidth_cap_gb: u64,
    #[serde(default = "default_retention_days")]
    history_retention_days: i64,
    // Set true the first time the window is closed-to-tray, so the
    // one-time "still running in the tray" notification never repeats
    // after that -- not a per-session flag, a permanent one.
    #[serde(default)]
    has_shown_tray_hint: bool,
}

fn default_retention_days() -> i64 {
    DEFAULT_RETENTION_DAYS
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: "English".to_string(),
            refresh_interval_sec: 2,
            notif_desktop: true,
            notif_sound: false,
            notif_tray: true,
            bandwidth_cap_enabled: false,
            bandwidth_cap_gb: 500,
            history_retention_days: DEFAULT_RETENTION_DAYS,
            has_shown_tray_hint: false,
        }
    }
}

fn settings_path() -> std::path::PathBuf {
    history::get_data_dir().join("settings.json")
}

#[tauri::command]
fn get_settings() -> AppSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Shared with the close-to-tray handler, which needs to persist the
/// `has_shown_tray_hint` flag without going through the `save_settings`
/// command (that needs `DashboardState` for its re-prune side effect,
/// which isn't relevant here).
fn write_settings_file(settings: &AppSettings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(), json).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_settings(settings: AppSettings, state: tauri::State<DashboardState>) -> Result<(), String> {
    write_settings_file(&settings)?;
    // Applied immediately rather than waiting for the next periodic prune
    // tick or app restart -- lowering the retention should take effect
    // right away, not silently next hour.
    state.history_reader.lock_ip().prune(settings.history_retention_days);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryStatsDto {
    db_path: String,
    row_count: i64,
}

#[tauri::command]
fn get_history_stats(state: tauri::State<DashboardState>) -> HistoryStatsDto {
    HistoryStatsDto {
        db_path: history::get_data_dir().join("network_history.db").display().to_string(),
        row_count: state.history_reader.lock_ip().row_count(),
    }
}

/// Permanently deletes all recorded telemetry history. The frontend is
/// responsible for confirming with the user first -- this has no undo.
#[tauri::command]
fn clear_history(state: tauri::State<DashboardState>) {
    state.history_reader.lock_ip().clear_all();
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DataUsage {
    downloaded: String,
    uploaded: String,
    total: String,
    cap_gb: u64,
    cap_enabled: bool,
    used_percent: f64,
}

/// Real bytes transferred since the start of the current calendar month —
/// backs Settings' Data Usage Cap card. `HistoryDb::usage_since` sums
/// consecutive-row diffs of the cumulative interface counters rather than
/// `last - first`, so an interface reset (reboot, NIC reconnect) partway
/// through the month can't produce a negative or wildly wrong total.
#[tauri::command]
fn get_data_usage(state: tauri::State<DashboardState>) -> DataUsage {
    let now = Local::now();
    let month_start = format!("{:04}-{:02}-01T00:00:00.000000", now.year(), now.month());
    let (dl, ul) = state.history_reader.lock_ip().usage_since(&month_start);
    let total = dl + ul;
    let settings = get_settings();
    let cap_bytes = settings.bandwidth_cap_gb as f64 * 1_000_000_000.0;
    let used_percent = if cap_bytes > 0.0 { (total / cap_bytes * 100.0).min(999.0) } else { 0.0 };
    DataUsage {
        downloaded: format_bytes_total(Some(dl)),
        uploaded: format_bytes_total(Some(ul)),
        total: format_bytes_total(Some(total)),
        cap_gb: settings.bandwidth_cap_gb,
        cap_enabled: settings.bandwidth_cap_enabled,
        used_percent,
    }
}

// — System tray — a real OS resource created/destroyed to match the
// "Show in system tray" setting, not just a stored flag. When the tray
// icon exists, closing the main window hides it instead of quitting
// (same convention as most tray-resident apps); when it doesn't, the
// window's close button quits normally. Left-click or the menu's
// "Show" both restore the window; "Quit" exits for real.
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        // `show()` + `set_focus()` alone isn't enough on every Linux WM:
        // if the window was minimized (not just hidden-to-tray),
        // `show()` doesn't implicitly unminimize it on GNOME/some X11
        // WMs, and even when it does, the WM can leave it behind
        // whatever's currently focused instead of raising it. The
        // always-on-top toggle forces a raise on X11 WMs that respect
        // it; on Wayland compositors that ignore unsolicited raise/focus
        // requests entirely (by design, not a bug this app can work
        // around), the taskbar/dock entry still flashes as attention-
        // requesting.
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.set_always_on_top(true);
        let _ = w.set_always_on_top(false);
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let show_item = MenuItem::with_id(app, "show", "Show Network Monitor", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("no default window icon configured in tauri.conf.json".to_string()))?;
    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("Network Monitor")
        // Real speed text beside the icon — set to a real value within
        // one poll tick (<=2s) by the background loop; "—" here is an
        // honest "no reading yet" placeholder, not a fake number.
        .title("—")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)
}

#[tauri::command]
fn set_tray_enabled(enabled: bool, app: AppHandle, tray_state: tauri::State<TrayState>) -> Result<(), String> {
    let mut slot = tray_state.0.lock_ip();
    if enabled {
        if slot.is_none() {
            *slot = Some(build_tray(&app).map_err(|e| e.to_string())?);
        }
    } else {
        *slot = None; // Dropping the TrayIcon removes it from the OS tray.
    }
    Ok(())
}

// — Alerts —

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AlertRow {
    id: u64,
    severity: String,
    message: String,
    time: String,
    read: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AlertsSnapshot {
    detection_enabled: bool,
    alerts: Vec<AlertRow>,
}

#[tauri::command]
fn get_alerts(state: tauri::State<DashboardState>) -> AlertsSnapshot {
    let s = state.shared.lock_ip();
    AlertsSnapshot {
        detection_enabled: s.detection_enabled,
        alerts: s
            .alerts
            .iter()
            .map(|a| AlertRow {
                id: a.id,
                severity: a.severity.label().to_string(),
                message: a.message.clone(),
                time: a.time.clone(),
                read: a.read,
            })
            .collect(),
    }
}

#[tauri::command]
fn set_anomaly_detection(enabled: bool, state: tauri::State<DashboardState>) {
    state.shared.lock_ip().detection_enabled = enabled;
}

#[tauri::command]
fn mark_all_alerts_read(state: tauri::State<DashboardState>) {
    for a in state.shared.lock_ip().alerts.iter_mut() {
        a.read = true;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be the first plugin registered (Tauri's own recommendation)
        // so it can intercept a second launch before anything else in the
        // chain runs. A second launch (double-clicking the app again, the
        // autostart entry firing while it's already running from a
        // previous login, etc.) hits this callback in the *first*
        // instance instead of spawning a duplicate process — the second
        // instance's `main()` exits right after this fires.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_notification::init())
        // Restores the window's real last size/position on launch instead
        // of always resetting to tauri.conf.json's 1300x900 default --
        // saved automatically on move/resize/close.
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let shared = Arc::new(Mutex::new(Shared { detection_enabled: true, ..Shared::default() }));
            let db_path = history::get_data_dir().join("network_history.db");

            // Bandwidth + connections + history poll — mirrors the egui
            // build's `AppState::spawn` 1s loop (relaxed to 2s here,
            // matching the design reference's default refresh interval).
            {
                let shared = Arc::clone(&shared);
                let db_path = db_path.clone();
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    let db = HistoryDb::open(&db_path);
                    let mut reader = NetworkReader::new();
                    let mut detector = AnomalyDetector::new();
                    let mut tick: u64 = 0;
                    // Re-prune roughly hourly against the user's current
                    // setting -- `save_settings` already re-prunes
                    // immediately when the setting changes, but a
                    // long-running process (no restart, setting never
                    // touched) still needs *some* periodic enforcement, not
                    // just the one-shot prune at DB-open time.
                    let prune_every_ticks = (3_600_000 / UPDATE_MS).max(1);
                    loop {
                        // A panic inside one tick used to kill this thread
                        // silently -- `Shared` would freeze at its last-good
                        // value forever with no error surfaced anywhere in
                        // the UI (the frontend's connection-health check
                        // only trips on invoke() rejections, never on a
                        // stale-but-still-responding backend). Catching it
                        // here keeps the loop (and telemetry) alive instead.
                        let tick_result = catch_unwind(AssertUnwindSafe(|| {
                        let reading = reader.get();
                        tick += 1;
                        if tick % prune_every_ticks == 0 {
                            db.prune(get_settings().history_retention_days);
                        }
                        let latency = shared.lock_ip().latency_ms;
                        let conns_count = netmon_core::connections::read_connections(500).len();
                        let combined_rate = reading.download_rate + reading.upload_rate;

                        // Live speed beside the tray icon (AppIndicator/KStatusNotifierItem
                        // label — GNOME needs the AppIndicator extension, already required
                        // for the tray icon itself to render at all). Skipped when the tray
                        // is off so this doesn't do pointless work every tick.
                        if let Some(tray_state) = app_handle.try_state::<TrayState>() {
                            if let Some(tray) = tray_state.0.lock_ip().as_ref() {
                                let label = format!(
                                    "↓ {} ↑ {}",
                                    format_bytes_rate(Some(reading.download_rate)),
                                    format_bytes_rate(Some(reading.upload_rate)),
                                );
                                let _ = tray.set_title(Some(label));
                            }
                        }

                        db.save(&history::Reading {
                            default_iface: reading.default_iface.clone(),
                            download_rate: Some(reading.download_rate),
                            upload_rate: Some(reading.upload_rate),
                            total_downloaded: Some(reading.total_downloaded),
                            total_uploaded: Some(reading.total_uploaded),
                            latency_ms: latency,
                        });

                        {
                            let mut s = shared.lock_ip();
                            s.peak_download = s.peak_download.max(reading.download_rate);
                            s.peak_upload = s.peak_upload.max(reading.upload_rate);
                            s.reading = reading;
                            s.connections_count = conns_count;
                            s.last_updated = Local::now().format("%H:%M:%S").to_string();
                        }

                        // Always feed the detector so the baseline stays
                        // warm even while detection is toggled off, but
                        // only raise an alert when enabled — mirrors the
                        // egui build's AppState::spawn.
                        let spike = detector.observe(combined_rate);
                        let detection_enabled = shared.lock_ip().detection_enabled;
                        if let Some((mean, value)) = spike.filter(|_| detection_enabled) {
                            let severity = if value > mean * 5.0 { Severity::Critical } else { Severity::Warning };
                            let message = format!(
                                "Traffic spike: {} vs a recent baseline of {}",
                                format_bytes_rate(Some(value)),
                                format_bytes_rate(Some(mean)),
                            );
                            {
                                let mut s = shared.lock_ip();
                                let id = s.next_alert_id;
                                s.next_alert_id += 1;
                                s.alerts.insert(
                                    0,
                                    AlertItem {
                                        id,
                                        severity,
                                        message: message.clone(),
                                        time: Local::now().format("%H:%M:%S").to_string(),
                                        read: false,
                                    },
                                );
                                s.alerts.truncate(ALERTS_KEPT);
                            }
                            // Real desktop notification, only if the user has
                            // that preference on — reads the same settings.json
                            // save_settings writes, so it stays in sync without
                            // threading a settings cache through Shared.
                            if get_settings().notif_desktop {
                                let _ = app_handle
                                    .notification()
                                    .builder()
                                    .title(format!("Network Monitor — {}", severity.label()))
                                    .body(&message)
                                    .show();
                            }
                        }
                        }));
                        if let Err(e) = tick_result {
                            let msg = e
                                .downcast_ref::<&str>()
                                .map(|s| s.to_string())
                                .or_else(|| e.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "unknown panic".to_string());
                            log::error!("bandwidth poll tick panicked, recovering: {msg}");
                        }

                        std::thread::sleep(Duration::from_millis(UPDATE_MS));
                    }
                });
            }

            // Latency poll, once every 5s — a `ping` round trip can take
            // up to a second, so it gets its own slower cadence rather
            // than blocking the bandwidth loop above.
            {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || loop {
                    let latency = netdev::measure_latency(netdev::LATENCY_TARGET, 1);
                    shared.lock_ip().latency_ms = latency;
                    std::thread::sleep(Duration::from_secs(LATENCY_INTERVAL_S));
                });
            }

            app.manage(DashboardState {
                shared,
                history_reader: Mutex::new(HistoryDb::open(&db_path)),
                connection_tracker: Mutex::new(netmon_core::connections::ConnectionRateTracker::new()),
            });

            // Cheap even when no database has been downloaded yet —
            // GeoIpReader::open() degrades to an inert None-backed
            // reader rather than failing setup.
            app.manage(GeoIpState(Mutex::new(netmon_core::geoip::GeoIpReader::open())));

            // Tray icon starts up matching the persisted setting, not
            // unconditionally — a user who turned it off shouldn't see
            // it reappear on every launch.
            let initial_tray = if get_settings().notif_tray { Some(build_tray(app.handle())?) } else { None };
            app.manage(TrayState(Mutex::new(initial_tray)));

            // Close button hides to tray when the tray icon exists,
            // quits normally when it doesn't — checked fresh on every
            // close so toggling the setting takes effect immediately.
            if let Some(window) = app.get_webview_window("main") {
                let app_handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        let has_tray = app_handle
                            .try_state::<TrayState>()
                            .map(|s| s.0.lock_ip().is_some())
                            .unwrap_or(false);
                        if has_tray {
                            api.prevent_close();
                            if let Some(w) = app_handle.get_webview_window("main") {
                                let _ = w.hide();
                            }
                            // One-time only, ever -- a user closing the
                            // window repeatedly shouldn't get renagged
                            // every time; the flag persists in
                            // settings.json once shown.
                            let mut settings = get_settings();
                            if !settings.has_shown_tray_hint {
                                let _ = app_handle
                                    .notification()
                                    .builder()
                                    .title("Network Monitor")
                                    .body("Still running in the system tray -- click the tray icon to reopen, or Quit from its menu to exit fully.")
                                    .show();
                                settings.has_shown_tray_hint = true;
                                let _ = write_settings_file(&settings);
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
            get_traffic_chart,
            get_live_metrics_chart,
            get_connections_page,
            get_processes,
            get_wifi,
            get_interfaces,
            get_vpn,
            get_reports,
            generate_report,
            remove_report,
            get_dns,
            run_ping,
            run_traceroute,
            run_speed_test,
            vpn_connect,
            vpn_disconnect,
            get_autostart,
            set_autostart,
            get_username,
            get_alerts,
            set_anomaly_detection,
            mark_all_alerts_read,
            run_packet_capture,
            run_port_scan,
            scan_firewall,
            scan_nethogs,
            get_settings,
            save_settings,
            get_history_stats,
            clear_history,
            set_tray_enabled,
            get_data_usage,
            get_geoip_status,
            download_geoip_database,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
