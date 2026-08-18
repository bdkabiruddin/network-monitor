use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct DnsStats {
    pub current_transactions: u64,
    pub total_transactions: u64,
    pub cache_size: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

impl DnsStats {
    pub fn cache_hit_rate_pct(&self) -> Option<f64> {
        let total = self.cache_hits + self.cache_misses;
        (total > 0).then(|| (self.cache_hits as f64 / total as f64) * 100.0)
    }
}

fn trailing_number(line: &str) -> Option<u64> {
    line.rsplit(':').next()?.trim().parse().ok()
}

/// Cumulative resolver counters via `resolvectl statistics`
/// (systemd-resolved) — real numbers since the resolver started, not a
/// live queries-per-minute feed (that would need a D-Bus subscription
/// this doesn't implement). Returns None if systemd-resolved isn't
/// running / `resolvectl` isn't installed.
pub fn read_dns_stats() -> Option<DnsStats> {
    let output = Command::new("resolvectl").arg("statistics").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut stats = DnsStats::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Current Transactions:") {
            stats.current_transactions = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Total Transactions:") {
            stats.total_transactions = rest.trim().parse().unwrap_or(0);
        } else if trimmed.starts_with("Current Cache Size:") {
            stats.cache_size = trailing_number(trimmed).unwrap_or(0);
        } else if trimmed.starts_with("Cache Hits:") {
            stats.cache_hits = trailing_number(trimmed).unwrap_or(0);
        } else if trimmed.starts_with("Cache Misses:") {
            stats.cache_misses = trailing_number(trimmed).unwrap_or(0);
        }
    }
    Some(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_rate_from_counts() {
        let stats = DnsStats { cache_hits: 30, cache_misses: 10, ..Default::default() };
        assert_eq!(stats.cache_hit_rate_pct(), Some(75.0));
    }

    #[test]
    fn hit_rate_none_when_no_queries_yet() {
        assert_eq!(DnsStats::default().cache_hit_rate_pct(), None);
    }
}
