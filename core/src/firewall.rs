use std::process::Command;

#[derive(Debug, Clone)]
pub struct FirewallRuleLine {
    pub text: String,
    pub handle: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FirewallChain {
    pub table: String,
    pub name: String,
    pub hook_info: Option<String>,
    pub rules: Vec<FirewallRuleLine>,
}

/// Reads the live nftables ruleset via `pkexec nft -a list ruleset` —
/// needs root, same on-demand-scan-with-a-password-prompt pattern as
/// the NetHogs scan, since polling this every few seconds would mean
/// re-prompting for a password constantly. Returns each chain with its
/// raw rule lines rather than decomposing them into
/// protocol/port/action columns: nftables' match-expression syntax is
/// too varied to map onto a handoff-shaped table without a real parser,
/// and a half-parsed column would misrepresent rules it doesn't fully
/// understand.
pub fn read_ruleset() -> Result<Vec<FirewallChain>, String> {
    let output =
        Command::new("pkexec").args(["nft", "-a", "list", "ruleset"]).output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if stderr.trim().is_empty() {
            "couldn't read the firewall ruleset — cancelled, or nftables not in use on this system".to_string()
        } else {
            stderr.trim().to_string()
        });
    }
    Ok(parse_ruleset(&String::from_utf8_lossy(&output.stdout)))
}

fn split_handle(line: &str) -> (String, Option<String>) {
    match line.find("# handle ") {
        Some(idx) => (line[..idx].trim().to_string(), Some(line[idx + "# handle ".len()..].trim().to_string())),
        None => (line.to_string(), None),
    }
}

enum Block {
    Table(String),
    Chain,
    /// Anything else opened directly inside a table — a `set`/`map`/
    /// `flowtable`/counter block, or any future nft construct this
    /// doesn't specifically know about. Its content is skipped, but
    /// crucially its *nesting* is still tracked, so its closing brace
    /// doesn't get mistaken for the table's own.
    Other,
}

/// Stack-based scan of `nft list ruleset` output, tracking real brace
/// nesting (counted per line, not assumed one level per line) rather
/// than a fixed 0/1/2+ depth. The previous version only recognized
/// `chain { }` blocks inside a table — any other named block (`set`,
/// `map`, `flowtable`, ...) had no nesting tracked for it at all, so
/// *its* closing `}` was mistaken for the table's, silently truncating
/// the rest of the ruleset (any chains after it went unparsed with no
/// error surfaced). A same-line balanced construct (an inline anonymous
/// set literal like `elements = { 10.0.0.1, 10.0.0.2 }`) has a net brace
/// delta of 0 and doesn't change nesting at all.
///
/// Still doesn't handle multi-line rules (an anonymous set/map spanning
/// several lines inside a single rule) — good enough for the common
/// case, not a full nftables grammar.
fn parse_ruleset(text: &str) -> Vec<FirewallChain> {
    let mut chains = Vec::new();
    let mut stack: Vec<Block> = Vec::new();
    let mut current: Option<FirewallChain> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;
        let net = opens - closes;

        if net == 0 {
            if matches!(stack.last(), Some(Block::Chain)) {
                if line.starts_with("type ") && line.contains("hook") {
                    if let Some(c) = current.as_mut() {
                        c.hook_info = Some(line.trim_end_matches(';').to_string());
                    }
                } else if let Some(c) = current.as_mut() {
                    let (text, handle) = split_handle(line);
                    c.rules.push(FirewallRuleLine { text, handle });
                }
            }
        } else if net > 0 {
            // Only the outermost brace opened by this line is
            // meaningfully classified -- nft's own pretty-printer opens
            // at most one named block per line in practice.
            let kind = match stack.last() {
                None => match line.strip_prefix("table ") {
                    Some(rest) => Block::Table(rest.trim_end_matches('{').trim().to_string()),
                    None => Block::Other,
                },
                Some(Block::Table(table)) => match line.strip_prefix("chain ") {
                    Some(rest) => {
                        let name = rest.trim_end_matches('{').trim().to_string();
                        current = Some(FirewallChain { table: table.clone(), name, hook_info: None, rules: Vec::new() });
                        Block::Chain
                    }
                    None => Block::Other,
                },
                _ => Block::Other,
            };
            for _ in 0..net {
                stack.push(match &kind {
                    Block::Table(t) => Block::Table(t.clone()),
                    Block::Chain => Block::Chain,
                    Block::Other => Block::Other,
                });
            }
        } else {
            for _ in 0..(-net) {
                if let Some(Block::Chain) = stack.pop() {
                    if let Some(c) = current.take() {
                        chains.push(c);
                    }
                }
            }
        }
    }
    chains
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_table_chain_and_rules() {
        let sample = r#"
table inet filter {
	chain input {
		type filter hook input priority filter; policy accept;
		iifname "lo" accept # handle 1
		tcp dport 22 accept # handle 3
	}
}
"#;
        let chains = parse_ruleset(sample);
        assert_eq!(chains.len(), 1);
        let chain = &chains[0];
        assert_eq!(chain.table, "inet filter");
        assert_eq!(chain.name, "input");
        assert!(chain.hook_info.as_deref().unwrap().contains("hook input"));
        assert_eq!(chain.rules.len(), 2);
        assert_eq!(chain.rules[0].text, "iifname \"lo\" accept");
        assert_eq!(chain.rules[0].handle.as_deref(), Some("1"));
        assert_eq!(chain.rules[1].handle.as_deref(), Some("3"));
    }

    #[test]
    fn handles_multiple_chains() {
        let sample = r#"
table inet filter {
	chain input {
		type filter hook input priority filter; policy drop;
		tcp dport 443 accept # handle 2
	}
	chain output {
		type filter hook output priority filter; policy accept;
	}
}
"#;
        let chains = parse_ruleset(sample);
        assert_eq!(chains.len(), 2);
        assert_eq!(chains[0].name, "input");
        assert_eq!(chains[1].name, "output");
        assert!(chains[1].rules.is_empty());
    }

    #[test]
    fn a_nested_set_block_does_not_truncate_the_rest_of_the_table() {
        let sample = r#"
table inet filter {
	set blocked_ips {
		type ipv4_addr
		elements = { 10.0.0.1, 10.0.0.2 }
	}
	chain input {
		type filter hook input priority filter; policy accept;
		ip saddr @blocked_ips drop # handle 4
	}
}
"#;
        let chains = parse_ruleset(sample);
        // Before the fix, the set block's closing `}` was mistaken for
        // the table's own, so `chain input` (which comes after it) was
        // never parsed at all.
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].name, "input");
        assert_eq!(chains[0].rules.len(), 1);
        assert_eq!(chains[0].rules[0].text, "ip saddr @blocked_ips drop");
        assert_eq!(chains[0].rules[0].handle.as_deref(), Some("4"));
    }
}
