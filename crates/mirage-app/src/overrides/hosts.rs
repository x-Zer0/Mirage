//! User hosts (iteration 2 §2): hosts-file text, written into the run
//! config's `hosts` only — the system hosts file is never touched.
use crate::domain::error::{AppError, AppResult};
use crate::storage::{atomic, layout::Layout};
use std::net::IpAddr;

const STAGE: &str = "overrides.hosts";
const MAX_LINES: usize = 2000;
const MAX_LINE_LEN: usize = 1024;
const MAX_DOMAIN_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;
/// Errors listed back to the user; the rest are counted.
const MAX_ERRORS: usize = 50;

/// Domain → addresses, in first-seen order; a domain on several lines keeps every address.
pub type HostEntries = Vec<(String, Vec<IpAddr>)>;

/// Saved text (comments kept) and its parsed entries.
#[derive(Debug, Default)]
pub struct Hosts {
    pub text: String,
    pub entries: HostEntries,
    /// Line errors of a hand-edited file; such a file applies no entries.
    pub errors: Vec<String>,
}

impl Hosts {
    pub fn load(layout: &Layout) -> AppResult<Self> {
        let text = match std::fs::read_to_string(layout.hosts_file()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(AppError::storage(STAGE, e)),
        };
        Ok(match parse(&text) {
            Ok(entries) => Self {
                text,
                entries,
                errors: Vec::new(),
            },
            Err(errors) => Self {
                text,
                entries: Vec::new(),
                errors,
            },
        })
    }

    pub fn save(layout: &Layout, text: &str) -> AppResult<()> {
        atomic::write(&layout.hosts_file(), text.as_bytes())
            .map_err(|e| AppError::storage(STAGE, e))
    }
}

/// `IP domain [domain…]` per line, `#` starts a comment. Errors carry line numbers.
pub fn parse(text: &str) -> Result<HostEntries, Vec<String>> {
    let mut entries: HostEntries = Vec::new();
    let mut errors = Vec::new();
    let mut lines = 0;
    for (index, line) in text.lines().enumerate() {
        let no = index + 1;
        let content = line.split('#').next().unwrap_or_default().trim();
        if content.is_empty() {
            continue;
        }
        lines += 1;
        if line.len() > MAX_LINE_LEN {
            errors.push(format!("第 {no} 行：超过 {MAX_LINE_LEN} 个字符"));
            continue;
        }
        let mut fields = content.split_whitespace();
        let Some(ip) = fields.next().and_then(|f| f.parse::<IpAddr>().ok()) else {
            errors.push(format!("第 {no} 行：IP 地址不合法"));
            continue;
        };
        let domains: Vec<&str> = fields.collect();
        if domains.is_empty() {
            errors.push(format!("第 {no} 行：缺少域名"));
            continue;
        }
        for domain in domains {
            if !valid_domain(domain) {
                errors.push(format!("第 {no} 行：域名“{domain}”不合法"));
                continue;
            }
            let domain = domain.to_ascii_lowercase();
            match entries.iter_mut().find(|(d, _)| *d == domain) {
                Some((_, ips)) if !ips.contains(&ip) => ips.push(ip),
                Some(_) => {}
                None => entries.push((domain, vec![ip])),
            }
        }
    }
    if lines > MAX_LINES {
        errors.insert(0, format!("最多 {MAX_LINES} 行，当前 {lines} 行"));
    }
    if errors.is_empty() {
        return Ok(entries);
    }
    if errors.len() > MAX_ERRORS {
        let more = errors.len() - MAX_ERRORS;
        errors.truncate(MAX_ERRORS);
        errors.push(format!("另有 {more} 处错误未列出"));
    }
    Err(errors)
}

/// Exact name, `*.` (one label) or `+.` (the name and every subdomain), as Mihomo matches them.
fn valid_domain(domain: &str) -> bool {
    let body = domain
        .strip_prefix("*.")
        .or_else(|| domain.strip_prefix("+."))
        .unwrap_or(domain);
    !body.is_empty()
        && domain.len() <= MAX_DOMAIN_LEN
        && body.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= MAX_LABEL_LEN
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}
