use std::collections::HashMap;
use std::process::Command;
use std::time::Instant;

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Connection {
    pub proto: String,
    pub state: String,
    pub local: String,
    pub peer: String,
    pub proc_name: String,
    pub pid: String,
    /// Real cumulative bytes sent/received, from `ss -i`'s per-socket
    /// TCP_INFO line — TCP only (UDP has no such counters exposed by
    /// `ss`), `None` for UDP rows and any TCP row `ss` didn't attach an
    /// info line to. `ss` itself has no connection-age/duration field
    /// anywhere in its output (`lastsnd`/`lastrcv`/`busy` are all about
    /// recency or cumulative active-time, not wall-clock age) — a real
    /// "how long have I been watching this connection" duration exists,
    /// but it's computed by [`ConnectionRateTracker`] across polls, not
    /// parsed from any single `ss` snapshot, so it isn't a field here.
    pub sent_bytes: Option<u64>,
    pub recv_bytes: Option<u64>,
}

fn ss_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"^(?P<proto>tcp|udp)\s+(?P<state>\S+)\s+\d+\s+\d+\s+(?P<local>\S+)\s+(?P<peer>\S+)(?:\s+users:\(\("(?P<proc>[^"]+)",pid=(?P<pid>\d+),fd=\d+\)\))?"#,
        )
        .unwrap()
    })
}

fn info_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"bytes_sent:(?P<sent>\d+).*?bytes_received:(?P<recv>\d+)").unwrap())
}

/// Active TCP/UDP sockets via `ss -tuinp` — the `-i` flag attaches a
/// second, indented line per TCP socket with real TCP_INFO fields
/// (`bytes_sent`/`bytes_received` among them); UDP sockets get no such
/// line. Process attribution (name, PID) is only available for the
/// current user's own sockets without root — other users' connections
/// show up with an empty process field.
pub fn read_connections(limit: usize) -> Vec<Connection> {
    let Ok(output) = Command::new("ss").args(["-tuinp"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let re = ss_line_re();
    let info_re = info_line_re();
    let mut rows: Vec<Connection> = Vec::new();
    for line in stdout.lines().skip(1) {
        // An indented continuation line (ss -i's TCP_INFO block) belongs
        // to the connection parsed on the previous line, not a new row.
        if line.starts_with(char::is_whitespace) {
            if let Some(last) = rows.last_mut() {
                if let Some(caps) = info_re.captures(line) {
                    last.sent_bytes = caps["sent"].parse().ok();
                    last.recv_bytes = caps["recv"].parse().ok();
                }
            }
            continue;
        }
        let Some(caps) = re.captures(line.trim()) else {
            continue;
        };
        if rows.len() >= limit {
            break;
        }
        rows.push(Connection {
            proto: caps["proto"].to_string(),
            state: caps["state"].to_string(),
            local: caps["local"].to_string(),
            peer: caps["peer"].to_string(),
            proc_name: caps.name("proc").map(|m| m.as_str().to_string()).unwrap_or_default(),
            pid: caps.name("pid").map(|m| m.as_str().to_string()).unwrap_or_default(),
            sent_bytes: None,
            recv_bytes: None,
        });
    }
    rows
}

#[derive(Debug, Clone, Copy)]
pub struct ConnectionRate {
    pub sent_bps: f64,
    pub recv_bps: f64,
}

/// What's tracked about a connection across polls: a real bytes/sec rate
/// (TCP only, needs two samples — `None` on first sighting) and a real
/// "how long has this app been observing it" duration (every proto,
/// every sighting, since it needs no counters at all).
///
/// `duration_secs` is honestly *not* the socket's true wall-clock age —
/// there's no field anywhere in `ss`'s output for that (only recency/
/// retransmit timers), and getting the real value needs root-level
/// `conntrack`. It's the time since this tracker instance first noticed
/// the connection, which undercounts anything that was already
/// established before the app started (or before its last restart).
/// Real and useful for "which of my current connections have been open
/// longest this session", just not a claim about pre-launch history.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionTrack {
    pub rate: Option<ConnectionRate>,
    pub duration_secs: f64,
}

struct TrackedEntry {
    first_seen: Instant,
    prev_bytes: Option<(u64, u64)>,
    prev_time: Instant,
    last_rate: Option<ConnectionRate>,
}

/// Below this, a diff against the baseline sample is numerically
/// unstable -- a shared tracker gets `update()` calls from the auto-
/// refresh timer, a manual refresh click, and a nav-page switch, none of
/// which are mutually exclusive; two landing milliseconds apart can turn
/// a few KB of jitter into an apparent tens-of-GB/s rate for one tick.
const MIN_RATE_DT_SECS: f64 = 0.25;

/// Persists across calls (one instance per poller, not per call) — both
/// the rate and the duration it computes need state carried from the
/// previous poll(s), not just the current snapshot `ss` returns.
#[derive(Default)]
pub struct ConnectionRateTracker {
    entries: HashMap<String, TrackedEntry>,
}

impl ConnectionRateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Public so callers can look up a connection's track using the same
    /// key `update`'s returned map is keyed by.
    pub fn key(c: &Connection) -> String {
        format!("{}|{}", c.local, c.peer)
    }

    /// Call once per poll tick with the freshly-fetched connection list.
    pub fn update(&mut self, conns: &[Connection]) -> HashMap<String, ConnectionTrack> {
        let now = Instant::now();
        let mut out = HashMap::with_capacity(conns.len());
        let mut next = HashMap::with_capacity(conns.len());
        for c in conns {
            let key = Self::key(c);
            let bytes_now = match (c.sent_bytes, c.recv_bytes) {
                (Some(s), Some(r)) => Some((s, r)),
                _ => None,
            };

            let prev = self.entries.get(&key);
            // A lower byte count than last time means this is actually a
            // new connection that reused the same local:peer tuple (e.g.
            // a reconnect) — treat it as a first sighting (fresh duration,
            // no rate) rather than reporting a nonsensical negative rate.
            let is_reconnect = matches!(
                (bytes_now, prev.and_then(|p| p.prev_bytes)),
                (Some((s, r)), Some((ps, pr))) if s < ps || r < pr
            );

            let first_seen = if is_reconnect { now } else { prev.map(|p| p.first_seen).unwrap_or(now) };

            // If the baseline sample is too fresh to diff against
            // reliably, reuse the last computed rate for this tick and
            // leave the baseline alone, rather than recompute over a
            // near-zero interval.
            let stale_enough = prev.map(|p| now.duration_since(p.prev_time).as_secs_f64() >= MIN_RATE_DT_SECS).unwrap_or(true);

            let rate = if is_reconnect {
                None
            } else if !stale_enough {
                prev.and_then(|p| p.last_rate)
            } else {
                match (bytes_now, prev.and_then(|p| p.prev_bytes), prev.map(|p| p.prev_time)) {
                    (Some((s, r)), Some((ps, pr)), Some(prev_time)) => {
                        let dt = now.duration_since(prev_time).as_secs_f64();
                        (dt > 0.0).then(|| ConnectionRate { sent_bps: (s - ps) as f64 / dt, recv_bps: (r - pr) as f64 / dt })
                    }
                    _ => None,
                }
            };

            out.insert(key.clone(), ConnectionTrack { rate, duration_secs: now.duration_since(first_seen).as_secs_f64() });

            let (prev_bytes, prev_time) = if is_reconnect || stale_enough {
                (bytes_now, now)
            } else {
                (prev.and_then(|p| p.prev_bytes), prev.map(|p| p.prev_time).unwrap_or(now))
            };
            next.insert(key, TrackedEntry { first_seen, prev_bytes, prev_time, last_rate: rate });
        }
        self.entries = next;
        out
    }
}

/// No-root proxy for "which processes are most network-active": counts
/// open connections per process from an already-fetched connection
/// list, ranked descending. Not a bandwidth figure — a process with one
/// connection streaming video can use far more bandwidth than one with
/// ten idle keep-alive connections, but this needs no elevated access.
pub fn top_processes_from(connections: &[Connection], limit: usize) -> Vec<(String, String, usize)> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for conn in connections {
        if conn.pid.is_empty() {
            continue;
        }
        *counts.entry((conn.pid.clone(), conn.proc_name.clone())).or_insert(0) += 1;
    }
    let mut rows: Vec<((String, String), usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows.truncate(limit);
    rows.into_iter().map(|((pid, proc_name), count)| (pid, proc_name, count)).collect()
}

/// Same as [`top_processes_from`], but fetches its own `ss -tuinp` snapshot
/// — for callers that don't already have a connection list in hand.
pub fn read_top_processes_by_connections(limit: usize) -> Vec<(String, String, usize)> {
    top_processes_from(&read_connections(1000), limit)
}

/// Strips the `:port` (or `[ipv6]:port`) suffix `ss -tuinp` reports a
/// peer address with, leaving a bare IP suitable for a GeoIP lookup.
/// Shared by both UI builds rather than duplicated.
pub fn peer_ip(peer: &str) -> &str {
    if let Some(rest) = peer.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        peer.rsplit_once(':').map(|(ip, _port)| ip).unwrap_or(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_line_with_process() {
        let line = r#"tcp   ESTAB  0    0   192.168.1.5:52134   140.82.112.3:443  users:(("firefox",pid=1234,fd=87))"#;
        let caps = ss_line_re().captures(line).expect("should match");
        assert_eq!(&caps["proto"], "tcp");
        assert_eq!(&caps["state"], "ESTAB");
        assert_eq!(caps.name("pid").unwrap().as_str(), "1234");
        assert_eq!(caps.name("proc").unwrap().as_str(), "firefox");
    }

    #[test]
    fn parses_udp_line_without_process() {
        let line = "udp   UNCONN  0    0   0.0.0.0:68   0.0.0.0:*";
        let caps = ss_line_re().captures(line).expect("should match");
        assert_eq!(&caps["proto"], "udp");
        assert!(caps.name("pid").is_none());
    }

    #[test]
    fn peer_ip_strips_ipv4_and_bracketed_ipv6_ports() {
        assert_eq!(peer_ip("140.82.112.3:443"), "140.82.112.3");
        assert_eq!(peer_ip("[2606:4700::6810:85e5]:443"), "2606:4700::6810:85e5");
        assert_eq!(peer_ip("192.168.1.5"), "192.168.1.5"); // no port at all
    }

    #[test]
    fn info_line_extracts_real_byte_counters() {
        let line = "\t cubic wscale:6,7 rto:205 rtt:4.442/1.105 ato:40 mss:1448 pmtu:1500 rcvmss:1448 advmss:1448 \
                     cwnd:10 bytes_sent:1708697 bytes_retrans:117 bytes_acked:1708581 bytes_received:1828425 \
                     segs_out:29399 segs_in:14974";
        let caps = info_line_re().captures(line).expect("should match");
        assert_eq!(&caps["sent"], "1708697");
        assert_eq!(&caps["recv"], "1828425");
    }

    fn conn(local: &str, peer: &str, sent: u64, recv: u64) -> Connection {
        Connection {
            proto: "tcp".to_string(),
            state: "ESTAB".to_string(),
            local: local.to_string(),
            peer: peer.to_string(),
            proc_name: String::new(),
            pid: String::new(),
            sent_bytes: Some(sent),
            recv_bytes: Some(recv),
        }
    }

    #[test]
    fn rate_tracker_needs_two_samples_before_reporting_a_rate() {
        let mut tracker = ConnectionRateTracker::new();
        let first = tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 1000, 500)]);
        let track = first.get("10.0.0.1:1|1.1.1.1:443").expect("tracked from the very first sighting");
        assert!(track.rate.is_none(), "no rate on the very first sighting of a connection");
        assert!(track.duration_secs < 0.1, "duration should start at ~0 on first sighting");

        // Must clear MIN_RATE_DT_SECS (see rate_tracker_reuses_the_last_rate_
        // when_polled_too_soon below) so this sample is treated as a real,
        // fresh diff rather than a too-close-together one.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let second = tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 6000, 3000)]);
        let track = second.get("10.0.0.1:1|1.1.1.1:443").expect("tracked on second sample");
        let rate = track.rate.expect("rate present on second sample");
        // 5000 bytes sent / ~0.3s should land somewhere around 16666 B/s —
        // loose bounds since real thread::sleep isn't exact.
        assert!(rate.sent_bps > 5_000.0, "sent_bps = {}", rate.sent_bps);
        assert!(rate.recv_bps > 2_000.0, "recv_bps = {}", rate.recv_bps);
        // Duration keeps growing from the *first* sighting, not reset by
        // the second poll.
        assert!(track.duration_secs > 0.25, "duration_secs = {}", track.duration_secs);
    }

    #[test]
    fn rate_tracker_reuses_the_last_rate_when_polled_too_soon() {
        // A shared tracker gets update() calls from more than one source
        // (auto-refresh timer, manual refresh click, nav switch) that
        // aren't mutually exclusive -- two landing close together must
        // not diff over a near-zero interval and report a wildly
        // inflated rate.
        let mut tracker = ConnectionRateTracker::new();
        tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 1_000, 500)]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let stable = tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 31_000, 500)]);
        let stable_rate = stable.get("10.0.0.1:1|1.1.1.1:443").unwrap().rate.expect("rate after a real interval");

        // Immediately poll again with a huge byte jump -- if this were
        // diffed against the ~0ms gap, it would report an absurd rate.
        let too_soon = tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 1_031_000, 500)]);
        let too_soon_rate = too_soon.get("10.0.0.1:1|1.1.1.1:443").unwrap().rate.expect("reuses the last rate, not None");

        assert_eq!(too_soon_rate.sent_bps, stable_rate.sent_bps, "must reuse the last computed rate, not recompute over ~0s");
    }

    #[test]
    fn rate_tracker_treats_a_byte_count_drop_as_a_reconnect_not_a_negative_rate() {
        let mut tracker = ConnectionRateTracker::new();
        tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 5000, 5000)]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Same local:peer tuple, but a much lower byte count — a real
        // reconnect reusing the same 4-tuple, not negative traffic.
        let rates = tracker.update(&[conn("10.0.0.1:1", "1.1.1.1:443", 100, 100)]);
        let track = rates.get("10.0.0.1:1|1.1.1.1:443").expect("still tracked");
        assert!(track.rate.is_none(), "a byte-count drop must not produce a rate at all");
        // Duration also resets — a reconnect is a fresh connection, not a
        // continuation of the old one's lifetime.
        assert!(track.duration_secs < 0.03, "duration_secs = {} (should have reset)", track.duration_secs);
    }
}
