use std::borrow::Borrow;

use regex::{Captures, Regex};
use thiserror::Error;

use crate::config::PrivacyConfig;
use crate::models::{AnalysisInput, CommandRecord, GitContext, TerminalContext};

#[derive(Debug, Clone)]
pub struct PrivacyRedactor {
    enabled: bool,
    replacement: String,
    rules: Vec<Regex>,
}

impl Default for PrivacyRedactor {
    fn default() -> Self {
        Self::new(PrivacyConfig::default()).expect("default privacy patterns are valid")
    }
}

impl PrivacyRedactor {
    /// Compile a redactor from configuration.
    ///
    /// # Errors
    ///
    /// Returns [`PrivacyError::InvalidPattern`] when a user-provided regular
    /// expression is invalid.
    pub fn new(config: impl Borrow<PrivacyConfig>) -> Result<Self, PrivacyError> {
        Self::from_config(config.borrow())
    }

    /// Compile a redactor from borrowed configuration.
    ///
    /// # Errors
    ///
    /// Returns [`PrivacyError::InvalidPattern`] when a user-provided regular
    /// expression is invalid.
    ///
    /// # Panics
    ///
    /// Panics only if a compile-time built-in regular expression is invalid.
    /// The expressions are covered by this crate's tests.
    pub fn from_config(config: &PrivacyConfig) -> Result<Self, PrivacyError> {
        let mut patterns = Vec::new();

        if config.redact_private_keys {
            patterns.extend([
                // PEM/OpenSSH private key blocks, including all line breaks.
                r"(?ms)-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----.*?-----END (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----",
                // Public SSH keys can reveal identity and hosts even though they
                // are not authentication secrets themselves.
                r"(?m)\b(?:ssh-(?:rsa|ed25519)|ecdsa-sha2-nistp\d+)\s+[A-Za-z0-9+/]{40,}={0,3}(?:\s+[^\r\n]+)?",
            ]);
        }
        if config.redact_authorization {
            patterns.extend([
                r"(?im)(?P<prefix>\bauthorization\s*:\s*(?:bearer|basic)\s+)(?P<secret>[^\s,;]+)",
                r"(?im)(?P<prefix>\bproxy-authorization\s*:\s*(?:bearer|basic)\s+)(?P<secret>[^\s,;]+)",
            ]);
        }
        if config.redact_cookies {
            patterns.extend([
                r"(?im)(?P<prefix>\bset-cookie\s*:\s*)(?P<secret>[^\r\n]+)",
                r"(?im)(?P<prefix>\bcookie\s*:\s*)(?P<secret>[^\r\n]+)",
            ]);
        }
        if config.redact_passwords {
            patterns.extend([
                // Password embedded in a database/service URI.
                r"(?i)(?P<prefix>\b[a-z][a-z0-9+.-]*://[^\s:/@]+:)(?P<secret>[^\s/@]+)(?P<suffix>@)",
                r#"(?im)(?P<prefix>\b(?:password|passwd|pwd|db_password|database_password)\s*(?:=|:)\s*["']?)(?P<secret>[^\s"';,]{3,})"#,
                r#"(?i)(?P<prefix>--(?:password|passwd)(?:=|\s+)["']?)(?P<secret>[^\s"']{3,})"#,
            ]);
        }
        if config.redact_api_keys {
            patterns.extend([
                // Common provider formats.  The thresholds intentionally avoid
                // matching prose such as `sk-example`.
                r"\bsk-[A-Za-z0-9_-]{20,}\b",
                r"\bAKIA[0-9A-Z]{16}\b",
                r"\b(?:AIza)[A-Za-z0-9_-]{30,}\b",
                r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{30,})\b",
                r"\b(?:xox[baprs]-[A-Za-z0-9-]{10,})\b",
                r#"(?im)(?P<prefix>\b(?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|client[_-]?secret)\s*(?:=|:)\s*["']?)(?P<secret>[^\s"';,]{6,})"#,
                r#"(?i)(?P<prefix>--(?:api-key|apikey|access-key|secret-key)(?:=|\s+)["']?)(?P<secret>[^\s"']{6,})"#,
            ]);
        }
        if config.redact_tokens {
            patterns.extend([
                // Three URL-safe Base64 segments are distinctive enough to
                // identify JWTs without decoding their content.
                r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b",
                r#"(?im)(?P<prefix>\b(?:access[_-]?token|refresh[_-]?token|auth[_-]?token|token)\s*(?:=|:)\s*["']?)(?P<secret>[^\s"';,]{6,})"#,
                r#"(?i)(?P<prefix>--(?:token|access-token|refresh-token)(?:=|\s+)["']?)(?P<secret>[^\s"']{6,})"#,
            ]);
        }
        if config.redact_env_values {
            patterns.push(
                r#"(?im)(?P<prefix>^(?:\+\s*)?(?:export\s+)?[A-Z][A-Z0-9_]*(?:PASSWORD|PASSWD|API_KEY|ACCESS_TOKEN|REFRESH_TOKEN|AUTH_TOKEN|SECRET|COOKIE)[A-Z0-9_]*\s*=\s*["']?)(?P<secret>[^\s"'#;]{3,})"#,
            );
        }

        let mut rules = patterns
            .into_iter()
            .map(|pattern| Regex::new(pattern).expect("built-in privacy regex is valid"))
            .collect::<Vec<_>>();
        for (index, pattern) in config.extra_patterns.iter().enumerate() {
            rules.push(
                Regex::new(pattern).map_err(|source| PrivacyError::InvalidPattern {
                    index,
                    pattern: pattern.clone(),
                    source,
                })?,
            );
        }

        Ok(Self {
            enabled: config.redaction,
            replacement: config.replacement.clone(),
            rules,
        })
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            replacement: "[REDACTED]".to_owned(),
            rules: Vec::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn redact(&self, input: &str) -> String {
        if !self.enabled || input.is_empty() {
            return input.to_owned();
        }
        let mut output = input.to_owned();
        for rule in &self.rules {
            output = rule
                .replace_all(&output, |captures: &Captures<'_>| {
                    let prefix = captures.name("prefix").map_or("", |value| value.as_str());
                    let suffix = captures.name("suffix").map_or("", |value| value.as_str());
                    format!("{prefix}{}{suffix}", self.replacement)
                })
                .into_owned();
        }
        output
    }

    pub fn redact_command_record(&self, record: &CommandRecord) -> CommandRecord {
        let mut redacted = record.clone();
        redacted.command = self.redact(&redacted.command);
        redacted.cwd = self.redact_path(&redacted.cwd);
        redacted.stdout = self.redact(&redacted.stdout);
        redacted.stderr = self.redact(&redacted.stderr);
        redacted
    }

    pub fn redact_git_context(&self, git: &GitContext) -> GitContext {
        let mut redacted = git.clone();
        redacted.repo_root = self.redact_path(&redacted.repo_root);
        redacted.branch = redacted.branch.as_deref().map(|branch| self.redact(branch));
        redacted.remote = redacted.remote.as_deref().map(|remote| self.redact(remote));
        redacted
    }

    pub fn redact_analysis_input(&self, input: &AnalysisInput) -> AnalysisInput {
        let mut redacted = input.clone();
        redacted.shell = self.redact(&redacted.shell);
        redacted.cwd = self.redact_path(&redacted.cwd);
        redacted.command = self.redact(&redacted.command);
        redacted.stdout = self.redact(&redacted.stdout);
        redacted.stderr = self.redact(&redacted.stderr);
        redacted.context = redacted
            .context
            .iter()
            .map(|record| self.redact_command_record(record))
            .collect();
        redacted.environment = self.redact_environment(&redacted.environment);
        redacted.environment_changes = redacted
            .environment_changes
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_deref()
                        .map(|value| self.redact_environment_value(key, value)),
                )
            })
            .collect();
        redacted.git = redacted
            .git
            .as_ref()
            .map(|git| self.redact_git_context(git));
        redacted
    }

    pub fn redact_terminal_context(&self, context: &TerminalContext) -> TerminalContext {
        let mut redacted = context.clone();
        redacted.cwd = self.redact_path(&redacted.cwd);
        redacted.shell = self.redact(&redacted.shell);
        redacted.commands = redacted
            .commands
            .iter()
            .map(|record| self.redact_command_record(record))
            .collect();
        redacted.environment = self.redact_environment(&redacted.environment);
        redacted.git = redacted
            .git
            .as_ref()
            .map(|git| self.redact_git_context(git));
        redacted
    }

    fn redact_path(&self, path: &std::path::Path) -> std::path::PathBuf {
        if !self.enabled {
            return path.to_path_buf();
        }
        std::path::PathBuf::from(self.redact(&path.to_string_lossy()))
    }

    fn redact_environment(
        &self,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        environment
            .iter()
            .map(|(key, value)| (key.clone(), self.redact_environment_value(key, value)))
            .collect()
    }

    fn redact_environment_value(&self, key: &str, value: &str) -> String {
        let assignment = self.redact(&format!("{key}={value}"));
        assignment
            .split_once('=')
            .map_or(assignment.as_str(), |(_, value)| value)
            .to_owned()
    }
}

#[derive(Debug, Error)]
pub enum PrivacyError {
    #[error("invalid privacy regex at extra_patterns[{index}] (`{pattern}`): {source}")]
    InvalidPattern {
        index: usize,
        pattern: String,
        #[source]
        source: regex::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> PrivacyRedactor {
        PrivacyRedactor::default()
    }

    #[test]
    fn redacts_provider_keys_without_hiding_normal_identifiers() {
        let synthetic_key = ["sk", "abcdefghijklmnopqrstuvwxyz123456"].join("-");
        let input = format!("key={synthetic_key} model=sk-example");
        let output = redactor().redact(&input);
        assert_eq!(output, "key=[REDACTED] model=sk-example");
    }

    #[test]
    fn redacts_credentials_from_paths_and_all_git_text_fields() {
        let secret = ["sk", "abcdefghijklmnopqrstuvwxyz123456"].join("-");
        let mut input = AnalysisInput::new("true", 1, format!("/Users/test/{secret}/project"));
        input
            .environment
            .insert("VIRTUAL_ENV".to_owned(), format!("/tmp/{secret}/venv"));
        input.environment_changes.insert(
            "VIRTUAL_ENV".to_owned(),
            Some(format!("/tmp/{secret}/changed")),
        );
        input.git = Some(GitContext {
            repo_root: format!("/tmp/{secret}/repo").into(),
            branch: Some(format!("feature/{secret}")),
            remote: Some(format!("https://example.invalid/{secret}/repo.git")),
            ..GitContext::default()
        });

        let redacted = redactor().redact_analysis_input(&input);
        let serialized = serde_json::to_string(&redacted).unwrap();
        assert!(!serialized.contains(&secret));
        assert!(serialized.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_assignments_flags_and_authorization_headers() {
        let authorization = ["Authorization: Bearer", "header.payload.signature"].join(" ");
        let input = format!(
            "API_KEY='super-secret-value'\ncurl --token abcdefghijk\n{authorization}\nCookie: session=verysecret; theme=dark"
        );
        let output = redactor().redact(&input);
        assert!(!output.contains("super-secret-value"));
        assert!(!output.contains("abcdefghijk"));
        assert!(!output.contains("header.payload.signature"));
        assert!(!output.contains("session=verysecret"));
        assert!(output.contains("API_KEY='[REDACTED]"));
    }

    #[test]
    fn redacts_jwt_database_password_and_env_output() {
        let jwt = [
            "eyJhbGciOiJIUzI1NiJ9",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            "signaturevalue",
        ]
        .join(".");
        let database_url = ["postgres://alice", "fixture-password@db.example/app"].join(":");
        let service_token = ["fixture", "service", "token"].join("-");
        let input = format!(
            "jwt={jwt}\nDATABASE_URL={database_url}\nexport SERVICE_AUTH_TOKEN={service_token}"
        );
        let output = redactor().redact(&input);
        assert!(!output.contains("eyJhbGci"));
        assert!(output.contains("postgres://alice:[REDACTED]@db.example/app"));
        assert!(!output.contains(&service_token));
    }

    #[test]
    fn redacts_multiline_private_and_ssh_keys() {
        let private_key = [
            "-----BEGIN OPENSSH",
            "PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH",
            "PRIVATE KEY-----",
        ]
        .join(" ");
        let input = format!(
            "{private_key}\nssh-ed25519 AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA me@mac"
        );
        let output = redactor().redact(&input);
        assert_eq!(output, "[REDACTED]\n[REDACTED]");
    }

    #[test]
    fn disabled_redaction_is_byte_for_byte_unchanged() {
        let config = PrivacyConfig {
            redaction: false,
            ..PrivacyConfig::default()
        };
        let redactor = PrivacyRedactor::new(config).unwrap();
        let secret = "password=hunter22\n";
        assert_eq!(redactor.redact(secret), secret);
    }

    #[test]
    fn custom_patterns_are_validated_and_applied() {
        let config = PrivacyConfig {
            extra_patterns: vec![r"customer-\d{6}".to_owned()],
            ..PrivacyConfig::default()
        };
        let redactor = PrivacyRedactor::new(config).unwrap();
        assert_eq!(redactor.redact("customer-123456"), "[REDACTED]");

        let invalid = PrivacyConfig {
            extra_patterns: vec!["(".to_owned()],
            ..PrivacyConfig::default()
        };
        assert!(matches!(
            PrivacyRedactor::new(invalid),
            Err(PrivacyError::InvalidPattern { index: 0, .. })
        ));
    }

    #[test]
    fn record_redaction_does_not_mutate_the_original() {
        let bearer = ["fixture", "bearer", "value"].join("-");
        let record = CommandRecord::completed(
            format!("curl -H 'Authorization: Bearer {bearer}'"),
            "/tmp",
            1,
            "",
            "password=hunter22",
        );
        let redacted = redactor().redact_command_record(&record);
        assert!(record.command.contains(&bearer));
        assert!(!redacted.command.contains(&bearer));
        assert!(!redacted.stderr.contains("hunter22"));
    }
}
