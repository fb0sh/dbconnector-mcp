use regex::Regex;
use std::sync::LazyLock;

static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]+:)([^@\s]+)@").expect("url password regex")
});

static QUERY_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(password|pwd|secret|token)=([^&\s]+)").expect("query secret regex")
});

/// Remove URL userinfo passwords and common secret query parameters from text
/// that might be returned to a client or written to a log.
pub fn redact_secrets(input: &str) -> String {
    let with_url = URL_PASSWORD.replace_all(input, "$1***@");
    QUERY_SECRET.replace_all(&with_url, "$1=***").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_url_passwords() {
        let raw = "connect postgres://app:s3cret@db.example:5432/app failed";
        let out = redact_secrets(raw);
        assert!(!out.contains("s3cret"));
        assert!(out.contains("***"));
        assert!(out.contains("db.example"));
    }

    #[test]
    fn redacts_query_passwords() {
        let out = redact_secrets("mongodb://h/?authSource=admin&password=s3cret");
        assert!(!out.contains("s3cret"));
    }
}
