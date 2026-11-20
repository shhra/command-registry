//! Redact secrets from command text and run output before they hit the database.
//!
//! Each rule names one capture group holding the secret. Values that are
//! references rather than literals (`$TOKEN`, `${TOKEN}`, `<TOKEN>`) are left
//! alone, so commands written with env-var references stay runnable.

use regex::{Captures, Regex};
use std::sync::LazyLock;

pub const REDACTED: &str = "«redacted»";

static RULES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Private key blocks.
        r"(?s)(-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----)",
        // Well-known token shapes.
        r"\b((?:gh[pousr]_)[A-Za-z0-9]{20,})",
        r"\b(github_pat_[A-Za-z0-9_]{20,})",
        r"\b(sk-[A-Za-z0-9_\-]{20,})",
        r"\b(xox[abprs]-[A-Za-z0-9\-]{10,})",
        r"\b(AKIA[0-9A-Z]{16})\b",
        r"\b(eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,})",
        // ENV-style assignments: API_KEY=..., DB_PASSWORD="..."
        r#"(?i)\b[A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|PASSWD|API_?KEY|ACCESS_?KEY|PRIVATE_?KEY|CREDENTIALS?)[A-Z0-9_]*=("[^"]*"|'[^']*'|[^\s"';&|]+)"#,
        // JSON-ish: "password": "..."
        r#"(?i)"[a-z0-9_\-]*(?:password|secret|token|api_?key)"\s*:\s*("[^"]*")"#,
        // CLI flags: --token X, --password=X, --api-key X
        r#"(?i)(?:^|\s)--?(?:password|passwd|token|secret|api[-_]?key|access[-_]?key|auth[-_]?token)(?:=|\s+)("[^"]*"|'[^']*'|[^\s"';&|]+)"#,
        // HTTP auth headers.
        r#"(?i)authorization:\s*(?:bearer|basic|token)\s+([^\s"']+)"#,
        // curl -u user:pass
        r#"(?:^|\s)(?:-u|--user)\s+['"]?[^:\s'"]+:([^\s'"@]+)"#,
        // scheme://user:pass@host
        r"[a-zA-Z][a-zA-Z0-9+.\-]*://[^:/\s@]+:([^@\s/]+)@",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid sanitize regex"))
    .collect()
});

static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").unwrap());

/// Returns the sanitized text and the number of redactions made.
pub fn sanitize(input: &str) -> (String, usize) {
    let mut text = input.to_string();
    let mut count = 0;
    for re in RULES.iter() {
        let replaced = re.replace_all(&text, |caps: &Captures| {
            let whole = caps.get(0).unwrap();
            let secret = caps.get(1).unwrap();
            if is_reference(secret.as_str()) {
                return whole.as_str().to_string();
            }
            count += 1;
            let start = secret.start() - whole.start();
            let end = secret.end() - whole.start();
            let s = secret.as_str();
            let quote = s.chars().next().filter(|c| (*c == '"' || *c == '\'') && s.len() >= 2 && s.ends_with(*c));
            let replacement = match quote {
                Some(q) => format!("{q}{REDACTED}{q}"),
                None => REDACTED.to_string(),
            };
            format!("{}{}{}", &whole.as_str()[..start], replacement, &whole.as_str()[end..])
        });
        text = replaced.into_owned();
    }
    (text, count)
}

/// Strip ANSI escapes and carriage-return overdraws so stored logs are plain text.
pub fn clean_output(input: &str) -> String {
    let no_ansi = ANSI.replace_all(input, "");
    no_ansi
        .split('\n')
        .map(|line| line.rsplit('\r').find(|s| !s.is_empty()).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_reference(value: &str) -> bool {
    let v = value.trim_matches(|c| c == '"' || c == '\'');
    v.starts_with('$') || (v.starts_with('<') && v.ends_with('>')) || v.contains(REDACTED) || v.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> String {
        sanitize(x).0
    }

    #[test]
    fn redacts_env_assignments() {
        assert_eq!(s("API_KEY=abc123 make deploy"), "API_KEY=«redacted» make deploy");
        assert_eq!(s(r#"DB_PASSWORD="hunter 2" psql"#), r#"DB_PASSWORD="«redacted»" psql"#);
    }

    #[test]
    fn keeps_env_references() {
        let cmd = r#"curl -H "Authorization: Bearer $GH_TOKEN" https://api.github.com"#;
        assert_eq!(s(cmd), cmd);
        assert_eq!(s("API_KEY=$API_KEY make"), "API_KEY=$API_KEY make");
        assert_eq!(s("tool --token ${TOKEN}"), "tool --token ${TOKEN}");
    }

    #[test]
    fn redacts_flags_headers_urls() {
        assert_eq!(s("tool --token abc --x 1"), "tool --token «redacted» --x 1");
        assert_eq!(s("tool --password=pw"), "tool --password=«redacted»");
        assert_eq!(s("curl -H 'Authorization: Bearer abc.def'"), "curl -H 'Authorization: Bearer «redacted»'");
        assert_eq!(s("git clone https://me:pw@host/repo"), "git clone https://me:«redacted»@host/repo");
        assert_eq!(s("curl -u admin:pw http://x"), "curl -u admin:«redacted» http://x");
    }

    #[test]
    fn redacts_token_shapes() {
        assert_eq!(s("echo ghp_abcdefghijklmnopqrstuvwxyz0123"), format!("echo {REDACTED}"));
        assert_eq!(s("x sk-ant-api03-abcdefghijklmnopqrstuv"), format!("x {REDACTED}"));
        assert_eq!(s("AKIAABCDEFGHIJKLMNOP"), REDACTED);
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        for cmd in [
            "cargo test --workspace",
            "ls -la ~/Projects",
            "grep -r token src/",
            r#"jq '.usage.input_tokens' out.json"#,
            "git log --author=sam",
        ] {
            assert_eq!(sanitize(cmd), (cmd.to_string(), 0), "{cmd}");
        }
    }

    #[test]
    fn cleans_ansi_and_cr() {
        assert_eq!(clean_output("\x1b[32mok\x1b[0m\nprog 10%\rprog 100%"), "ok\nprog 100%");
    }
}
