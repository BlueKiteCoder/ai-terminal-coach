use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// All on-disk locations used by the product.
///
/// The product specification intentionally uses `~/.config/aicoach` even on
/// macOS instead of `~/Library/Application Support` so the configuration is
/// easy to inspect and works naturally with dotfile managers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPaths {
    pub home_dir: PathBuf,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
    pub socket_file: PathBuf,
    pub logs_dir: PathBuf,
    pub history_file: PathBuf,
    pub window_state_file: PathBuf,
}

impl ProductPaths {
    /// Discover paths using the current user's home directory.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::HomeDirectoryUnavailable`] when the operating
    /// system cannot identify a home directory.
    pub fn discover() -> Result<Self, ConfigError> {
        dirs::home_dir()
            .map(Self::from_home)
            .ok_or(ConfigError::HomeDirectoryUnavailable)
    }

    pub fn from_home(home_dir: impl Into<PathBuf>) -> Self {
        let home_dir = home_dir.into();
        let config_dir = home_dir.join(".config").join("aicoach");
        let data_dir = home_dir.join(".aicoach");
        let run_dir = data_dir.join("run");
        Self {
            config_file: config_dir.join("config.toml"),
            logs_dir: data_dir.join("logs"),
            history_file: data_dir.join("history.json"),
            window_state_file: data_dir.join("window-state.json"),
            socket_file: run_dir.join("aicoach.sock"),
            home_dir,
            config_dir,
            data_dir,
            run_dir,
        }
    }

    /// Create all directories required for config, IPC, logs, and product data.
    ///
    /// # Errors
    ///
    /// Returns an I/O error annotated with the directory that could not be
    /// created.
    pub fn ensure_directories(&self) -> Result<(), ConfigError> {
        for directory in [
            &self.config_dir,
            &self.data_dir,
            &self.run_dir,
            &self.logs_dir,
        ] {
            fs::create_dir_all(directory).map_err(|source| ConfigError::Io {
                path: directory.clone(),
                source,
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(
                    |source| ConfigError::Io {
                        path: directory.clone(),
                        source,
                    },
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ai: AiConfig,
    pub coach: CoachConfig,
    pub keybindings: KeybindingsConfig,
    pub safety: SafetyConfig,
    pub privacy: PrivacyConfig,
    pub context: ContextConfig,
    pub history: HistoryConfig,
    pub window: WindowConfig,
}

impl Config {
    /// Load the standard `~/.config/aicoach/config.toml` file.
    ///
    /// # Errors
    ///
    /// Returns an I/O, TOML parse, or configuration validation error.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(ProductPaths::discover()?.config_file)
    }

    /// Load and validate a config file at an explicit path.
    ///
    /// # Errors
    ///
    /// Returns an I/O, TOML parse, or configuration validation error.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Self = toml::from_str(&source).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Load the standard config, creating a secure default file and product
    /// directories if this is the first run.
    ///
    /// # Errors
    ///
    /// Returns an I/O, serialization, parse, or validation error. An existing
    /// malformed file is never overwritten.
    pub fn load_or_create() -> Result<Self, ConfigError> {
        let paths = ProductPaths::discover()?;
        paths.ensure_directories()?;
        Self::load_or_create_at(paths.config_file)
    }

    /// Load a config at `path`, or atomically create the default when absent.
    ///
    /// # Errors
    ///
    /// Returns an I/O, serialization, parse, or validation error. An existing
    /// malformed file is never overwritten.
    pub fn load_or_create_at(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        match Self::load_from(path) {
            Ok(config) => Ok(config),
            Err(ConfigError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                let config = Self::default();
                config.save_to(path)?;
                Ok(config)
            }
            Err(error) => Err(error),
        }
    }

    /// Validate and atomically save to the standard config path.
    ///
    /// # Errors
    ///
    /// Returns a discovery, validation, serialization, or I/O error.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(ProductPaths::discover()?.config_file)
    }

    /// Validate then atomically replace a config file.  On Unix, newly-created
    /// files are owner-only because config commonly names sensitive env vars.
    ///
    /// # Errors
    ///
    /// Returns a validation, serialization, or path-annotated I/O error.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        self.validate()?;
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let encoded = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml");
        let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

        let write_result = (|| -> Result<(), ConfigError> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|source| ConfigError::Io {
                path: temporary.clone(),
                source,
            })?;
            file.write_all(encoded.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|source| ConfigError::Io {
                    path: temporary.clone(),
                    source,
                })?;
            fs::rename(&temporary, path).map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }

    /// Check all cross-field invariants and user-supplied regex patterns.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Validation`] containing every detected problem.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();
        self.ai.validate(&mut errors);

        if !matches!(self.coach.language.trim(), "en-US" | "zh-CN") {
            errors.push("coach.language must be \"en-US\" or \"zh-CN\"".to_owned());
        }
        if self.keybindings.completion.trim().is_empty() {
            errors.push("keybindings.completion must not be empty".to_owned());
        }
        if self.keybindings.chat.trim().is_empty() {
            errors.push("keybindings.chat must not be empty".to_owned());
        }
        if self.keybindings.risk_lens.trim().is_empty() {
            errors.push("keybindings.risk_lens must not be empty".to_owned());
        }
        if self.keybindings.toggle_coach.trim().is_empty() {
            errors.push("keybindings.toggle_coach must not be empty".to_owned());
        }
        let bindings = [
            (
                "completion",
                canonical_keybinding(&self.keybindings.completion),
            ),
            ("chat", canonical_keybinding(&self.keybindings.chat)),
            (
                "risk_lens",
                canonical_keybinding(&self.keybindings.risk_lens),
            ),
            (
                "toggle_coach",
                canonical_keybinding(&self.keybindings.toggle_coach),
            ),
        ];
        for (name, binding) in &bindings {
            if matches!(binding.as_slice(), [b'\t' | b'\r' | b'\n' | 0x12]) {
                errors.push(format!(
                    "keybindings.{name} must not override native Tab, Enter, or Ctrl+R"
                ));
            }
        }
        for (index, (left_name, left)) in bindings.iter().enumerate() {
            for (right_name, right) in &bindings[index + 1..] {
                if left == right {
                    errors.push(format!(
                        "keybindings.{left_name} and keybindings.{right_name} must differ"
                    ));
                }
            }
        }
        if self.context.max_commands == 0 {
            errors.push("context.max_commands must be greater than zero".to_owned());
        }
        if self.safety.mode != SafetyMode::Warn {
            errors.push("this release supports safety.mode = \"warn\" only".to_owned());
        }
        if self.context.max_output_per_command == 0 {
            errors.push("context.max_output_per_command must be greater than zero".to_owned());
        }
        if self.context.max_total_chars < self.context.max_output_per_command {
            errors
                .push("context.max_total_chars must be at least max_output_per_command".to_owned());
        }
        if self.history.enabled && self.history.max_messages == 0 {
            errors.push("history.max_messages must be greater than zero when enabled".to_owned());
        }
        if self.window.width < 40 || self.window.height < 10 {
            errors.push("window must be at least 40 columns by 10 rows".to_owned());
        }
        if self
            .window
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.trim().is_empty())
        {
            errors.push("window.terminal must not be empty when specified".to_owned());
        }
        if self.privacy.replacement.is_empty() {
            errors.push("privacy.replacement must not be empty".to_owned());
        }
        for (index, pattern) in self.privacy.extra_patterns.iter().enumerate() {
            if regex::Regex::new(pattern).is_err() {
                errors.push(format!(
                    "privacy.extra_patterns[{index}] is not a valid regex"
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Validation(errors))
        }
    }
}

fn canonical_keybinding(specification: &str) -> Vec<u8> {
    match specification.trim().to_ascii_lowercase().as_str() {
        "option+tab" => return vec![0x1b, b'\t'],
        "option+/" | "option+slash" => return vec![0x1b, b'/'],
        "option+r" => return vec![0x1b, b'r'],
        "option+space" => return vec![0x1b, b' '],
        _ => {}
    }

    let bytes = specification.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'^' {
        return vec![if bytes[1] == b'?' {
            0x7f
        } else {
            bytes[1].to_ascii_uppercase() & 0x1f
        }];
    }
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = usize::from(bytes.starts_with(b"^[")) * 2;
    if index == 2 {
        output.push(0x1b);
    }
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            match bytes[index + 1] {
                b't' => output.push(b'\t'),
                b'e' => output.push(0x1b),
                b'r' => output.push(b'\r'),
                b'n' => output.push(b'\n'),
                b'\\' => output.push(b'\\'),
                other => {
                    output.push(b'\\');
                    output.push(other);
                }
            }
            index += 2;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    output
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub provider: String,
    pub base_url: String,
    pub api_key_env: String,
    pub temperature: f32,
    pub models: AiModels,
    #[serde(alias = "timeouts")]
    pub timeouts_ms: AiTimeouts,
    pub max_concurrent_requests: usize,
}

impl AiConfig {
    fn validate(&self, errors: &mut Vec<String>) {
        let provider = self.provider.trim();
        if !matches!(provider, "openai-compatible" | "disabled" | "none") {
            errors.push(
                "ai.provider must be \"openai-compatible\", \"disabled\", or \"none\"".to_owned(),
            );
        }
        let base_url = self.base_url.trim();
        let provider_enabled = provider == "openai-compatible";
        if provider_enabled && base_url.is_empty() {
            errors.push(
                "ai.base_url is required when ai.provider is \"openai-compatible\"".to_owned(),
            );
        } else if !base_url.is_empty()
            && !url::Url::parse(base_url).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
        {
            errors.push(
                "ai.base_url must be an http(s) URL without credentials, query, or fragment"
                    .to_owned(),
            );
        }
        let mut env_chars = self.api_key_env.chars();
        let valid_env_start = env_chars
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
        let valid_env_rest =
            env_chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
        if !valid_env_start || !valid_env_rest {
            errors.push("ai.api_key_env must be a valid environment variable name".to_owned());
        }
        for (name, model) in [
            ("completion", &self.models.completion),
            ("error_analysis", &self.models.error_analysis),
            ("chat", &self.models.chat),
        ] {
            if provider_enabled && model.trim().is_empty() {
                errors.push(format!("ai.models.{name} must not be empty"));
            }
        }
        if !(0.0..=2.0).contains(&self.temperature) || !self.temperature.is_finite() {
            errors.push("ai.temperature must be between 0 and 2".to_owned());
        }
        for (name, timeout) in [
            ("completion", self.timeouts_ms.completion),
            ("error_analysis", self.timeouts_ms.error_analysis),
            ("chat", self.timeouts_ms.chat),
        ] {
            if timeout == 0 {
                errors.push(format!("ai.timeouts_ms.{name} must be greater than zero"));
            }
        }
        if self.max_concurrent_requests == 0 {
            errors.push("ai.max_concurrent_requests must be greater than zero".to_owned());
        }
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "disabled".to_owned(),
            base_url: String::new(),
            api_key_env: "AI_COACH_API_KEY".to_owned(),
            temperature: 0.2,
            models: AiModels::default(),
            timeouts_ms: AiTimeouts::default(),
            max_concurrent_requests: 4,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AiModels {
    pub completion: String,
    pub error_analysis: String,
    pub chat: String,
}

/// Compatibility alias for callers that prefer an explicit `Config` suffix.
pub type AiModelsConfig = AiModels;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AiTimeouts {
    pub completion: u64,
    pub error_analysis: u64,
    pub chat: u64,
}

pub type AiTimeoutsConfig = AiTimeouts;

impl Default for AiTimeouts {
    fn default() -> Self {
        Self {
            completion: 2_500,
            error_analysis: 12_000,
            chat: 90_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CoachConfig {
    pub language: String,
    pub inline_hint: bool,
    pub auto_error_analysis: bool,
}

impl Default for CoachConfig {
    fn default() -> Self {
        Self {
            language: "en-US".to_owned(),
            inline_hint: true,
            auto_error_analysis: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct KeybindingsConfig {
    pub completion: String,
    pub chat: String,
    pub risk_lens: String,
    pub toggle_coach: String,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            completion: "^[\\t".to_owned(),
            chat: "^[/".to_owned(),
            risk_lens: "Option+R".to_owned(),
            toggle_coach: "Option+Space".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SafetyMode {
    #[default]
    Warn,
    Confirm,
    Block,
}

impl fmt::Display for SafetyMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Warn => "warn",
            Self::Confirm => "confirm",
            Self::Block => "block",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SafetyConfig {
    pub enabled: bool,
    pub mode: SafetyMode,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: SafetyMode::Warn,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct PrivacyConfig {
    pub redaction: bool,
    /// Permit best-effort Terminal.app/iTerm2 screen-tail capture when a failed
    /// command did not provide explicit stdout/stderr through shell IPC.
    pub capture_screen_tail: bool,
    pub redact_api_keys: bool,
    pub redact_tokens: bool,
    pub redact_passwords: bool,
    pub redact_authorization: bool,
    pub redact_cookies: bool,
    pub redact_private_keys: bool,
    pub redact_env_values: bool,
    pub replacement: String,
    pub extra_patterns: Vec<String>,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            // Sending terminal output to a remote model is the high-risk edge;
            // safe-by-default redaction can still be explicitly disabled.
            redaction: true,
            capture_screen_tail: false,
            redact_api_keys: true,
            redact_tokens: true,
            redact_passwords: true,
            redact_authorization: true,
            redact_cookies: true,
            redact_private_keys: true,
            redact_env_values: true,
            replacement: "[REDACTED]".to_owned(),
            extra_patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ContextConfig {
    pub max_commands: usize,
    pub max_output_per_command: usize,
    pub max_total_chars: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_commands: 30,
            max_output_per_command: 20_000,
            max_total_chars: 100_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub max_messages: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_messages: 50,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    pub width: u16,
    pub height: u16,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub terminal: Option<String>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 100,
            height: 32,
            x: Some(120),
            y: Some(90),
            terminal: Some("auto".to_owned()),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine the user's home directory")]
    HomeDirectoryUnavailable,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("invalid configuration: {}", .0.join("; "))]
    Validation(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_paths_follow_the_documented_layout() {
        let paths = ProductPaths::from_home("/Users/tester");
        assert_eq!(
            paths.config_file,
            Path::new("/Users/tester/.config/aicoach/config.toml")
        );
        assert_eq!(
            paths.socket_file,
            Path::new("/Users/tester/.aicoach/run/aicoach.sock")
        );
        assert_eq!(paths.logs_dir, Path::new("/Users/tester/.aicoach/logs"));
    }

    #[test]
    fn default_config_is_valid_and_contains_no_secret() {
        let config = Config::default();
        config.validate().unwrap();
        let encoded = toml::to_string(&config).unwrap();
        assert!(encoded.contains("api_key_env"));
        assert!(!encoded.contains("api_key ="));
    }

    #[test]
    fn partial_config_uses_defaults() {
        let config: Config = toml::from_str(
            r#"
                [ai]
                base_url = "https://example.test/v1"

                [privacy]
                redaction = false

                [keybindings]
                completion = "^[\\t"
                chat = "^[/"
                toggle_coach = "Option+Space"
            "#,
        )
        .unwrap();
        assert_eq!(config.ai.base_url, "https://example.test/v1");
        assert_eq!(config.context.max_commands, 30);
        assert!(!config.privacy.redaction);
        assert_eq!(config.keybindings.risk_lens, "Option+R");
    }

    #[test]
    fn load_or_create_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/config.toml");
        let created = Config::load_or_create_at(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(created, loaded);
    }

    #[test]
    fn malformed_existing_config_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "not = [valid").unwrap();
        assert!(matches!(
            Config::load_or_create_at(&path),
            Err(ConfigError::Parse { .. })
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "not = [valid");
    }

    #[test]
    fn validation_reports_all_actionable_errors() {
        let mut config = Config::default();
        config.ai.base_url = "file:///tmp/model".to_owned();
        config.ai.api_key_env = "9INVALID".to_owned();
        config.ai.models.chat.clear();
        config.context.max_commands = 0;
        config.context.max_total_chars = 1;
        config.privacy.extra_patterns.push("(".to_owned());
        let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
            panic!("expected validation failure");
        };
        assert!(errors.len() >= 5);
    }

    #[test]
    fn validation_rejects_unsupported_safety_mode_and_credentialed_url() {
        let mut config = Config::default();
        config.safety.mode = SafetyMode::Block;
        config.ai.base_url = "https://user:password@example.test/v1?debug=true".to_owned();
        let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
            panic!("expected validation failure");
        };
        assert!(errors.iter().any(|error| error.contains("safety.mode")));
        assert!(errors.iter().any(|error| error.contains("credentials")));
    }

    #[test]
    fn validation_rejects_unknown_provider_and_malformed_http_url() {
        let mut config = Config::default();
        config.ai.provider = "opneai".to_owned();
        config.ai.base_url = "https://[invalid/v1".to_owned();
        let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
            panic!("expected validation failure");
        };
        assert!(errors.iter().any(|error| error.contains("ai.provider")));
        assert!(errors.iter().any(|error| error.contains("ai.base_url")));
    }

    #[test]
    fn validation_detects_equivalent_keybinding_spellings() {
        let mut config = Config::default();
        config.keybindings.completion = "Option+Tab".to_owned();
        config.keybindings.chat = "^[\\t".to_owned();
        let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
            panic!("expected validation failure");
        };
        assert!(errors.iter().any(|error| error.contains("must differ")));

        let mut config = Config::default();
        config.keybindings.risk_lens = "Option+Space".to_owned();
        let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
            panic!("expected Risk Lens shortcut collision")
        };
        assert!(errors.iter().any(|error| error.contains("must differ")));
    }

    #[test]
    fn validation_rejects_native_zsh_keybindings() {
        for reserved in ["^M", "^J", "^R", "\\t"] {
            let mut config = Config::default();
            config.keybindings.completion = reserved.to_owned();
            let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
                panic!("expected validation failure for {reserved}");
            };
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("must not override"))
            );
        }
        let mut config = Config::default();
        config.keybindings.completion = "Option+Tab".to_owned();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn default_configuration_is_provider_neutral_and_english() {
        let config = Config::default();
        assert_eq!(config.ai.provider, "disabled");
        assert!(config.ai.base_url.is_empty());
        assert!(config.ai.models.completion.is_empty());
        assert!(config.ai.models.error_analysis.is_empty());
        assert!(config.ai.models.chat.is_empty());
        assert_eq!(config.coach.language, "en-US");
        assert_eq!(config.ai.timeouts_ms.chat, 90_000);
        assert!(config.privacy.redaction);
    }

    #[test]
    fn language_is_limited_to_english_and_chinese() {
        for language in ["en-US", "zh-CN"] {
            let mut config = Config::default();
            config.coach.language = language.to_owned();
            config.validate().unwrap();
        }
        let mut config = Config::default();
        config.coach.language = "fr-FR".to_owned();
        let ConfigError::Validation(errors) = config.validate().unwrap_err() else {
            panic!("expected unsupported language to fail validation")
        };
        assert!(errors.iter().any(|error| error.contains("coach.language")));
    }

    #[test]
    fn packaged_configuration_exactly_matches_rust_defaults() {
        let packaged: Config =
            toml::from_str(include_str!("../../../config/default.toml")).unwrap();
        assert_eq!(packaged, Config::default());
    }

    #[test]
    fn unknown_fields_are_rejected_to_catch_typos() {
        let error = toml::from_str::<Config>("[coach]\ninlien_hint = true").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[cfg(unix)]
    #[test]
    fn new_config_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        Config::default().save_to(&path).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn product_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let paths = ProductPaths::from_home(directory.path());
        paths.ensure_directories().unwrap();
        for path in [
            paths.config_dir,
            paths.data_dir,
            paths.run_dir,
            paths.logs_dir,
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
