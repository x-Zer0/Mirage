//! Text redaction shared by logs, error details and diagnostics (DESIGN.md §14.3).

use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

struct Rule {
    pattern: Regex,
    replacement: &'static str,
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let rule = |pattern: &str, replacement| Rule {
        pattern: Regex::new(pattern).expect("valid redaction pattern"),
        replacement,
    };
    vec![
        // Credential-like query parameters: ?token=..., &key=...
        rule(
            r#"(?i)([?&](?:token|key|auth|password|passwd|pwd|sid|secret|uuid)=)[^&#\s'"]+"#,
            "${1}***",
        ),
        // Authorization headers, with or without a Bearer scheme.
        rule(
            r#"(?i)(authorization"?\s*[:=]\s*"?)(?:bearer\s+)?[^\s,'"}]+"#,
            "${1}***",
        ),
        // Secret-bearing fields in key=value, YAML or JSON form.
        rule(
            r#"(?i)("?\b(?:auth_data|auth_token|secret|password|passwd|private-key|psk|auth-str|uuid|subscribe_url)"?\s*[:=]\s*"?)[^\s,'"}]+"#,
            "${1}***",
        ),
        // E-mail addresses keep their first character and domain.
        rule(
            r"\b([A-Za-z0-9._%+-])[A-Za-z0-9._%+-]*@([A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,})\b",
            "${1}***@${2}",
        ),
    ]
});

/// Returns `input` with known secret shapes masked. Borrowed when nothing matched.
pub fn redact_text(input: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(input);
    for rule in RULES.iter() {
        if let Cow::Owned(replaced) = rule.pattern.replace_all(&out, rule.replacement) {
            out = Cow::Owned(replaced);
        }
    }
    out
}

/// A value that must never reach a log line, e.g. a full subscription URL.
/// `Debug` prints a placeholder; read it with [`SecretString::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(***)")
    }
}
