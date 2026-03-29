//! Best-effort secret redaction for execution logs and output previews.
//!
//! Applies regex-based pattern matching to scrub common secret formats
//! (API keys, tokens, PEM headers, generic `password=` assignments) from
//! text before it is persisted to log files or the SQLite `output_preview`
//! column.  Redaction is intentionally applied only at persistence
//! boundaries — in-memory `BackendOutput` fields remain unredacted so
//! response parsing is unaffected.

use regex::Regex;

use crate::config::types::OrchestrationConfig;

/// Regex-based secret scrubber.
///
/// Each pattern is a `(name, regex)` pair.  When a match is found the
/// matched text is replaced with `[REDACTED:<name>]`.
pub struct Redactor {
    patterns: Vec<(String, Regex)>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.patterns.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("Redactor")
            .field("pattern_count", &self.patterns.len())
            .field("pattern_names", &names)
            .finish()
    }
}

impl Redactor {
    /// Construct a redactor with the built-in pattern set.
    pub fn new() -> Self {
        Self {
            patterns: builtin_patterns(),
        }
    }

    /// Construct a redactor with built-in patterns plus caller-supplied
    /// extra regexes.  Invalid extra patterns are silently skipped (logged
    /// at warn level).
    pub fn with_custom_patterns(extra: &[String]) -> Self {
        let mut patterns = builtin_patterns();
        for (i, raw) in extra.iter().enumerate() {
            match Regex::new(raw) {
                Ok(re) => patterns.push((format!("custom_{}", i), re)),
                Err(e) => {
                    tracing::warn!(pattern = %raw, error = %e, "ignoring invalid custom redaction pattern");
                }
            }
        }
        Self { patterns }
    }

    /// Build a `Redactor` from orchestration config, returning `None` when
    /// redaction is explicitly disabled (`redact_secrets: false`).
    pub fn from_config(config: &OrchestrationConfig) -> Option<Self> {
        if !config.redact_secrets.unwrap_or(true) {
            return None;
        }
        let extra = config.redaction_patterns.as_deref().unwrap_or(&[]);
        if extra.is_empty() {
            Some(Self::new())
        } else {
            Some(Self::with_custom_patterns(extra))
        }
    }

    /// Replace all secret matches in `input` with `[REDACTED:<name>]`.
    pub fn redact(&self, input: &str) -> String {
        let mut out = input.to_string();
        for (name, re) in &self.patterns {
            let replacement = format!("[REDACTED:{}]", name);
            out = re.replace_all(&out, replacement.as_str()).into_owned();
        }
        out
    }

    /// Check whether redaction is enabled for the given config value.
    /// Defaults to `true` when `None`.
    pub fn is_enabled(redact_secrets: Option<bool>) -> bool {
        redact_secrets.unwrap_or(true)
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

/// Built-in secret patterns.  Order matters: more specific patterns are
/// listed before the generic catch-all to avoid partial matches.
fn builtin_patterns() -> Vec<(String, Regex)> {
    // Each entry: (label used in [REDACTED:<label>], regex pattern).
    // unwrap() is acceptable here — these are compile-time constant patterns.
    vec![
        (
            "aws_key".into(),
            Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        ),
        (
            "github_token".into(),
            Regex::new(r"gh[ps]_[A-Za-z0-9_]{36,}").unwrap(),
        ),
        (
            "github_oauth".into(),
            Regex::new(r"gho_[A-Za-z0-9_]{36,}").unwrap(),
        ),
        (
            "stripe_key".into(),
            Regex::new(r"sk_(live|test)_[A-Za-z0-9]{24,}").unwrap(),
        ),
        (
            "anthropic_key".into(),
            Regex::new(r"sk-ant-[A-Za-z0-9_\-]{20,}").unwrap(),
        ),
        (
            "openai_key".into(),
            Regex::new(r"sk-[A-Za-z0-9_\-]{20,}").unwrap(),
        ),
        (
            "bearer_token".into(),
            Regex::new(r"Bearer\s+[A-Za-z0-9._\-]{20,}").unwrap(),
        ),
        (
            "pem_key".into(),
            Regex::new(r"-----BEGIN\s+(RSA\s+)?PRIVATE\s+KEY-----").unwrap(),
        ),
        (
            "generic_secret".into(),
            Regex::new(
                r"(?i)(password|secret|token|api_key|apikey)\s*[=:]\s*['\x22]?[A-Za-z0-9/+_\-]{16,}",
            )
            .unwrap(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_aws_key() {
        let r = Redactor::new();
        let input = "key is AKIAIOSFODNN7EXAMPLE ok";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:aws_key]"), "got: {}", out);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn test_redact_github_token() {
        let r = Redactor::new();
        let input = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijkl";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:github_token]"), "got: {}", out);
    }

    #[test]
    fn test_redact_github_oauth() {
        let r = Redactor::new();
        let input = "oauth gho_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijkl";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:github_oauth]"), "got: {}", out);
    }

    #[test]
    fn test_redact_stripe_key() {
        let r = Redactor::new();
        let input = "PLACEHOLDER_STRIPE_LIVE_TESTING";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:stripe_key]"), "got: {}", out);
    }

    #[test]
    fn test_redact_stripe_test_key() {
        let r = Redactor::new();
        let input = "PLACEHOLDER_STRIPE_TEST_TESTING";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:stripe_key]"), "got: {}", out);
    }

    #[test]
    fn test_redact_openai_key() {
        let r = Redactor::new();
        let input = "OPENAI_API_KEY=sk-proj-xxxxxxxxxxxxxxxxxxxx";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:openai_key]"), "got: {}", out);
    }

    #[test]
    fn test_redact_anthropic_key() {
        let r = Redactor::new();
        let input = "key=sk-ant-api03-xxxxxxxxxxxxxxxxxxxxxxxxxx";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:anthropic_key]"), "got: {}", out);
    }

    #[test]
    fn test_redact_bearer_token() {
        let r = Redactor::new();
        let input = "Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6Ik";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:bearer_token]"), "got: {}", out);
    }

    #[test]
    fn test_redact_pem_key() {
        let r = Redactor::new();
        let input = "-----BEGIN RSA PRIVATE KEY-----";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:pem_key]"), "got: {}", out);

        let input2 = "-----BEGIN PRIVATE KEY-----";
        let out2 = r.redact(input2);
        assert!(out2.contains("[REDACTED:pem_key]"), "got: {}", out2);
    }

    #[test]
    fn test_redact_generic_secret_password() {
        let r = Redactor::new();
        let input = "password=abcdefghijklmnopqrstuvwxyz";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:generic_secret]"), "got: {}", out);
    }

    #[test]
    fn test_redact_generic_secret_variants() {
        let r = Redactor::new();

        let cases = [
            "secret=abcdefghijklmnop",
            "token: abcdefghijklmnop",
            "api_key='abcdefghijklmnop'",
            "APIKEY=abcdefghijklmnop",
        ];
        for case in &cases {
            let out = r.redact(case);
            assert!(
                out.contains("[REDACTED:generic_secret]"),
                "expected redaction for '{}', got: {}",
                case,
                out
            );
        }
    }

    #[test]
    fn test_preserves_non_secret_text() {
        let r = Redactor::new();
        let input = "Hello world, this is a normal log line with no secrets.";
        let out = r.redact(input);
        assert_eq!(out, input);
    }

    #[test]
    fn test_custom_patterns() {
        let extra = vec![r"CUSTOM-[0-9]{8}".to_string()];
        let r = Redactor::with_custom_patterns(&extra);
        let input = "my custom token CUSTOM-12345678 here";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:custom_0]"), "got: {}", out);
        assert!(!out.contains("CUSTOM-12345678"));
    }

    #[test]
    fn test_invalid_custom_pattern_skipped() {
        let extra = vec![r"[invalid".to_string()];
        let r = Redactor::with_custom_patterns(&extra);
        // Should still have built-in patterns and not panic
        let input = "AKIAIOSFODNN7EXAMPLE";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:aws_key]"), "got: {}", out);
    }

    #[test]
    fn test_multiple_secrets_in_one_line() {
        let r = Redactor::new();
        let input = "aws=AKIAIOSFODNN7EXAMPLE token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijkl";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED:aws_key]"), "got: {}", out);
        assert!(out.contains("[REDACTED:github_token]"), "got: {}", out);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn test_redact_secrets_disabled() {
        assert!(!Redactor::is_enabled(Some(false)));
    }

    #[test]
    fn test_redact_secrets_enabled_by_default() {
        assert!(Redactor::is_enabled(None));
        assert!(Redactor::is_enabled(Some(true)));
    }

    #[test]
    fn test_from_config_disabled() {
        let config = OrchestrationConfig {
            redact_secrets: Some(false),
            ..Default::default()
        };
        assert!(Redactor::from_config(&config).is_none());
    }

    #[test]
    fn test_from_config_enabled_default() {
        let config = OrchestrationConfig::default();
        let r = Redactor::from_config(&config);
        assert!(r.is_some());
    }

    #[test]
    fn test_from_config_with_custom() {
        let config = OrchestrationConfig {
            redaction_patterns: Some(vec![r"MY_TOKEN_[A-Z]{10}".to_string()]),
            ..Default::default()
        };
        let r = Redactor::from_config(&config).unwrap();
        let out = r.redact("found MY_TOKEN_ABCDEFGHIJ in output");
        assert!(out.contains("[REDACTED:custom_0]"), "got: {}", out);
    }
}
