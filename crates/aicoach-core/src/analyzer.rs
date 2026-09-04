use regex::Regex;
use serde::{Deserialize, Serialize};

pub use crate::models::AnalysisCategory;
use crate::models::{AnalysisInput, AnalysisResult, Severity};
use crate::safety::{RiskLevel, SafetyEngine};

const COMMON_COMMANDS: &[&str] = &[
    "awk", "brew", "cargo", "cat", "cd", "chmod", "chown", "clang", "clear", "cp", "curl",
    "docker", "echo", "find", "git", "grep", "head", "kill", "less", "ls", "make", "mkdir", "mv",
    "node", "npm", "npx", "open", "pip", "pip3", "pnpm", "pwd", "python", "python3", "rg", "rm",
    "rsync", "rustc", "sed", "ssh", "tail", "tar", "tree", "vim", "which", "xargs", "yarn", "zsh",
];

const GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "bisect",
    "branch",
    "checkout",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "diff",
    "fetch",
    "init",
    "log",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "remote",
    "reset",
    "restore",
    "revert",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
    "worktree",
];

const DOCKER_SUBCOMMANDS: &[&str] = &[
    "build",
    "compose",
    "container",
    "cp",
    "exec",
    "image",
    "images",
    "info",
    "inspect",
    "kill",
    "login",
    "logs",
    "network",
    "pause",
    "port",
    "ps",
    "pull",
    "push",
    "restart",
    "rm",
    "rmi",
    "run",
    "start",
    "stats",
    "stop",
    "system",
    "tag",
    "top",
    "version",
    "volume",
];

const COMPOSE_SUBCOMMANDS: &[&str] = &[
    "build", "config", "cp", "create", "down", "events", "exec", "images", "kill", "logs", "ls",
    "pause", "port", "ps", "pull", "push", "restart", "rm", "run", "start", "stop", "top",
    "unpause", "up", "version", "wait", "watch",
];

const PACKAGE_SUBCOMMANDS: &[&str] = &[
    "add",
    "audit",
    "ci",
    "install",
    "link",
    "list",
    "outdated",
    "publish",
    "remove",
    "run",
    "search",
    "test",
    "uninstall",
    "update",
    "upgrade",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalAnalysis {
    pub needs_ai: bool,
    pub category: AnalysisCategory,
    pub severity: Severity,
    pub title: String,
    pub message: String,
    pub suggested_command: Option<String>,
    pub confidence: f32,
}

impl LocalAnalysis {
    pub fn no_action() -> Self {
        Self {
            needs_ai: false,
            category: AnalysisCategory::Unknown,
            severity: Severity::Info,
            title: String::new(),
            message: String::new(),
            suggested_command: None,
            confidence: 1.0,
        }
    }

    pub fn needs_response(&self) -> bool {
        self.needs_ai || self.category != AnalysisCategory::Unknown
    }

    pub fn into_result(self) -> AnalysisResult {
        self.into()
    }
}

impl From<LocalAnalysis> for AnalysisResult {
    fn from(local: LocalAnalysis) -> Self {
        Self {
            need_response: local.needs_response(),
            severity: local.severity,
            category: local.category,
            title: local.title,
            message: local.message,
            suggested_command: local.suggested_command,
            confidence: local.confidence,
        }
    }
}

#[derive(Debug)]
pub struct LocalAnalyzer {
    safety: SafetyEngine,
    missing_command: Regex,
    git_bad_subcommand: Regex,
}

impl Default for LocalAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalAnalyzer {
    /// Construct the built-in analyzer.
    ///
    /// # Panics
    ///
    /// Panics only if a compile-time built-in regular expression is invalid.
    /// The expressions are covered by this crate's tests.
    pub fn new() -> Self {
        Self {
            safety: SafetyEngine::new(),
            missing_command: Regex::new(
                r#"(?im)(?:command not found|unknown command|not a recognized command)\s*:?\s*['`"]?([A-Za-z0-9_.+-]+)"#,
            )
            .expect("built-in command-not-found regex is valid"),
            git_bad_subcommand: Regex::new(
                r"(?im)git:\s*['`]([^'`]+)['`]\s+is not a git command",
            )
            .expect("built-in git regex is valid"),
        }
    }

    pub fn with_safety(safety: SafetyEngine) -> Self {
        Self {
            safety,
            ..Self::new()
        }
    }

    /// Fast, deterministic analysis.  `needs_ai` means the daemon may ask a
    /// provider for a richer explanation; local title/message/suggestion remain
    /// useful when offline.
    #[allow(clippy::too_many_lines)]
    pub fn analyze(&self, input: &AnalysisInput) -> LocalAnalysis {
        let command = input.command.trim();
        if command.is_empty() {
            return LocalAnalysis::no_action();
        }

        let safety = self.safety.assess(command);
        if safety.level >= RiskLevel::Medium {
            let message = safety.findings.first().map_or_else(
                || "This command can make destructive changes.".to_owned(),
                |finding| finding.message.clone(),
            );
            return LocalAnalysis {
                needs_ai: false,
                category: AnalysisCategory::DangerousCommand,
                severity: severity_for_risk(safety.level),
                title: format!("{} risk command", safety.level),
                message,
                suggested_command: None,
                confidence: 1.0,
            };
        }

        let output = input.combined_output();
        let output_lower = output.to_ascii_lowercase();
        let tokens = shell_words::split(command).unwrap_or_default();
        let executable = effective_executable(&tokens);

        if !self.missing_command.is_match(&output)
            && let Some(correction) = command_spelling_correction(command, &tokens)
        {
            let (category, title) = match executable.as_str() {
                "git" => (AnalysisCategory::Git, "Git subcommand appears misspelled"),
                "docker" => (
                    AnalysisCategory::Docker,
                    "Docker argument appears misspelled",
                ),
                "brew" | "npm" | "pnpm" | "yarn" | "pip" | "pip3" => (
                    AnalysisCategory::PackageManager,
                    "Package-manager subcommand appears misspelled",
                ),
                _ => (AnalysisCategory::Spelling, "Command appears misspelled"),
            };
            return issue(
                category,
                Severity::Warning,
                title,
                "A local spelling match is available.",
                Some(correction),
                false,
                0.94,
            );
        }

        // Output-based detectors are intentionally gated on a failed exit code;
        // `echo 'permission denied'` must never trigger an error analysis.
        if input.exit_code == 0 {
            return LocalAnalysis::no_action();
        }

        if let Some(captures) = self.git_bad_subcommand.captures(&output) {
            let invalid = captures.get(1).map_or("", |value| value.as_str());
            if let Some(valid) = closest_word(invalid, GIT_SUBCOMMANDS) {
                return issue(
                    AnalysisCategory::Git,
                    Severity::Warning,
                    "Git subcommand appears misspelled",
                    &format!("`{invalid}` is probably `{valid}`."),
                    Some(replace_word(command, invalid, valid)),
                    false,
                    0.99,
                );
            }
        }

        if let Some(captures) = self.missing_command.captures(&output) {
            let missing = captures.get(1).map_or("", |value| value.as_str());
            let suggestion = closest_word(missing, COMMON_COMMANDS)
                .map(|valid| replace_word(command, missing, valid));
            let message = suggestion.as_ref().map_or_else(
                || format!("The `{missing}` executable is not available on PATH."),
                |_| format!("`{missing}` closely matches an installed-style command name."),
            );
            return issue(
                AnalysisCategory::CommandNotFound,
                Severity::Warning,
                "Command not found",
                &message,
                suggestion,
                true,
                if closest_word(missing, COMMON_COMMANDS).is_some() {
                    0.96
                } else {
                    0.99
                },
            );
        }

        if is_ssh_error(&executable, &output_lower) {
            return issue(
                AnalysisCategory::Ssh,
                Severity::Error,
                "SSH connection failed",
                "SSH reported an authentication, host-key, name-resolution, or connection error.",
                None,
                true,
                0.98,
            );
        }

        if contains_any(
            &output_lower,
            &[
                "permission denied",
                "operation not permitted",
                "eacces",
                "access denied",
            ],
        ) {
            return issue(
                AnalysisCategory::PermissionDenied,
                Severity::Error,
                "Permission denied",
                "The current user or process does not have the required access.",
                None,
                true,
                0.99,
            );
        }

        if is_network_error(&output_lower) {
            return issue(
                AnalysisCategory::Network,
                Severity::Error,
                "Network request failed",
                "The output indicates a DNS, connection, TLS, or timeout failure.",
                None,
                true,
                0.97,
            );
        }

        if executable == "git" || looks_like_git_error(&output_lower) {
            return issue(
                AnalysisCategory::Git,
                Severity::Error,
                "Git command failed",
                "Git returned an error that may need repository context.",
                None,
                true,
                0.92,
            );
        }

        if executable == "docker" || looks_like_docker_error(&output_lower) {
            return issue(
                AnalysisCategory::Docker,
                Severity::Error,
                "Docker command failed",
                "Docker returned an engine, image, container, or Compose error.",
                None,
                true,
                0.92,
            );
        }

        if is_compiler_error(&executable, command, &output_lower) {
            return issue(
                AnalysisCategory::Compiler,
                Severity::Error,
                "Build or compiler error",
                "The output contains a compiler, linker, or syntax error.",
                None,
                true,
                0.96,
            );
        }

        if is_package_manager_error(&executable, &output_lower) {
            return issue(
                AnalysisCategory::PackageManager,
                Severity::Error,
                "Package manager failed",
                "Dependency installation or resolution failed.",
                None,
                true,
                0.94,
            );
        }

        if contains_any(
            &output_lower,
            &[
                "no such file or directory",
                "cannot stat",
                "cannot access",
                "does not exist",
                "enoent",
                "file not found",
            ],
        ) {
            return issue(
                AnalysisCategory::FileNotFound,
                Severity::Error,
                "File or directory not found",
                "A referenced path does not exist or was typed incorrectly.",
                None,
                true,
                0.98,
            );
        }

        // Any non-zero command can still benefit from AI, even when a local
        // detector cannot safely label it.
        issue(
            AnalysisCategory::Unknown,
            Severity::Error,
            "Command failed",
            "The command exited unsuccessfully and needs further analysis.",
            None,
            true,
            0.70,
        )
    }

    pub fn analyze_result(&self, input: &AnalysisInput) -> AnalysisResult {
        self.analyze(input).into()
    }

    pub fn should_analyze(&self, input: &AnalysisInput) -> bool {
        self.analyze(input).needs_response()
    }
}

fn issue(
    category: AnalysisCategory,
    severity: Severity,
    title: &str,
    message: &str,
    suggested_command: Option<String>,
    needs_ai: bool,
    confidence: f32,
) -> LocalAnalysis {
    LocalAnalysis {
        needs_ai,
        category,
        severity,
        title: title.to_owned(),
        message: message.to_owned(),
        suggested_command,
        confidence,
    }
}

fn severity_for_risk(risk: RiskLevel) -> Severity {
    match risk {
        RiskLevel::Low => Severity::Info,
        RiskLevel::Medium => Severity::Warning,
        RiskLevel::High => Severity::Error,
        RiskLevel::Critical => Severity::Critical,
    }
}

fn effective_executable(tokens: &[String]) -> String {
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let base = token
            .rsplit('/')
            .next()
            .unwrap_or(token)
            .to_ascii_lowercase();
        if base == "sudo" || base == "command" || base == "builtin" || base == "nohup" {
            index += 1;
            while index < tokens.len() && tokens[index].starts_with('-') {
                index += 1;
            }
            continue;
        }
        if base == "env" {
            index += 1;
            while index < tokens.len()
                && (tokens[index].contains('=') || tokens[index].starts_with('-'))
            {
                index += 1;
            }
            continue;
        }
        if token.contains('=') && !token.starts_with('=') {
            index += 1;
            continue;
        }
        return base;
    }
    String::new()
}

fn command_spelling_correction(command: &str, tokens: &[String]) -> Option<String> {
    let executable = effective_executable(tokens);
    if executable.is_empty() {
        return None;
    }

    if executable == "git" {
        let index = tokens
            .iter()
            .position(|token| token.rsplit('/').next() == Some("git"))?
            + 1;
        let candidate = tokens.get(index)?;
        if !candidate.starts_with('-') && !GIT_SUBCOMMANDS.contains(&candidate.as_str()) {
            let valid = closest_word(candidate, GIT_SUBCOMMANDS)?;
            return Some(replace_word(command, candidate, valid));
        }
    }

    if executable == "docker" {
        let docker_index = tokens
            .iter()
            .position(|token| token.rsplit('/').next() == Some("docker"))?;
        let candidate = tokens.get(docker_index + 1)?;
        if candidate == "compose" {
            let compose_command = tokens.get(docker_index + 2)?;
            if !compose_command.starts_with('-')
                && !COMPOSE_SUBCOMMANDS.contains(&compose_command.as_str())
            {
                let valid = closest_word(compose_command, COMPOSE_SUBCOMMANDS)?;
                return Some(replace_word(command, compose_command, valid));
            }
        } else if !candidate.starts_with('-') && !DOCKER_SUBCOMMANDS.contains(&candidate.as_str()) {
            let valid = closest_word(candidate, DOCKER_SUBCOMMANDS)?;
            return Some(replace_word(command, candidate, valid));
        }

        if tokens.iter().any(|token| token == "ps") {
            for option in tokens.iter().filter(|token| token.starts_with("--")) {
                if option == "--forma" {
                    return Some(replace_word(command, option, "--format"));
                }
            }
        }
    }

    if matches!(
        executable.as_str(),
        "brew" | "npm" | "pnpm" | "yarn" | "pip" | "pip3"
    ) {
        let executable_index = tokens.iter().position(|token| {
            token
                .rsplit('/')
                .next()
                .is_some_and(|base| base == executable)
        })?;
        let candidate = tokens.get(executable_index + 1)?;
        if !candidate.starts_with('-') && !PACKAGE_SUBCOMMANDS.contains(&candidate.as_str()) {
            let valid = closest_word(candidate, PACKAGE_SUBCOMMANDS)?;
            return Some(replace_word(command, candidate, valid));
        }
    }

    let first = tokens.first()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    if !base.contains('/') && !COMMON_COMMANDS.contains(&base) {
        let valid = closest_word(base, COMMON_COMMANDS)?;
        return Some(replace_word(command, base, valid));
    }
    None
}

fn closest_word<'a>(input: &str, candidates: &'a [&str]) -> Option<&'a str> {
    if input.len() < 3 {
        return None;
    }
    let maximum = if input.chars().count() <= 4 { 1 } else { 2 };
    candidates
        .iter()
        .map(|candidate| (*candidate, damerau_levenshtein(input, candidate)))
        .filter(|(_, distance)| *distance <= maximum)
        .min_by_key(|(_, distance)| *distance)
        .map(|(candidate, _)| candidate)
}

fn damerau_levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut matrix = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, cell) in matrix[0].iter_mut().enumerate() {
        *cell = index;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let substitution = usize::from(left[i - 1] != right[j - 1]);
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + substitution);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                matrix[i][j] = matrix[i][j].min(matrix[i - 2][j - 2] + 1);
            }
        }
    }
    matrix[left.len()][right.len()]
}

fn replace_word(command: &str, old: &str, new: &str) -> String {
    let Some(start) = command.match_indices(old).find_map(|(index, _)| {
        let before = command[..index].chars().next_back();
        let end = index + old.len();
        let after = command[end..].chars().next();
        let boundary = |character: Option<char>| {
            character.is_none_or(|value| {
                value.is_whitespace() || matches!(value, ';' | '|' | '&' | '(' | ')' | '\'' | '"')
            })
        };
        (boundary(before) && boundary(after)).then_some(index)
    }) else {
        return command.to_owned();
    };
    let mut result = command.to_owned();
    result.replace_range(start..start + old.len(), new);
    result
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn is_network_error(output: &str) -> bool {
    contains_any(
        output,
        &[
            "could not resolve host",
            "name or service not known",
            "temporary failure in name resolution",
            "connection refused",
            "connection reset",
            "network is unreachable",
            "operation timed out",
            "connection timed out",
            "request timeout",
            "tls handshake",
            "ssl certificate problem",
            "certificate verify failed",
        ],
    )
}

fn is_ssh_error(executable: &str, output: &str) -> bool {
    let ssh_command = matches!(executable, "ssh" | "scp" | "sftp")
        || (executable == "rsync" && output.contains("ssh"));
    let strong_signature = contains_any(
        output,
        &[
            "permission denied (publickey",
            "host key verification failed",
            "remote host identification has changed",
            "ssh_exchange_identification",
        ],
    );
    strong_signature
        || (ssh_command
            && contains_any(
                output,
                &[
                    "connection closed by",
                    "connection refused",
                    "could not resolve hostname",
                    "no route to host",
                ],
            ))
}

fn looks_like_git_error(output: &str) -> bool {
    output.starts_with("fatal:")
        || output.contains("not a git repository")
        || output.contains("pathspec")
        || output.contains("merge conflict")
        || output.contains("failed to push some refs")
}

fn looks_like_docker_error(output: &str) -> bool {
    contains_any(
        output,
        &[
            "cannot connect to the docker daemon",
            "error response from daemon",
            "no such container",
            "no such image",
            "docker: error",
            "unknown docker command",
            "is not a docker command",
        ],
    )
}

fn is_compiler_error(executable: &str, command: &str, output: &str) -> bool {
    let compiler_command = matches!(
        executable,
        "rustc" | "clang" | "clang++" | "gcc" | "g++" | "make"
    ) || command.starts_with("cargo build")
        || command.starts_with("cargo check")
        || command.contains(" run build");
    compiler_command
        || contains_any(
            output,
            &[
                "compilation failed",
                "could not compile",
                "undefined reference",
                "linker command failed",
                "syntax error:",
                "error[e0",
                "fatal error:",
            ],
        )
}

fn is_package_manager_error(executable: &str, output: &str) -> bool {
    let package_manager = matches!(
        executable,
        "brew" | "npm" | "npx" | "pnpm" | "yarn" | "pip" | "pip3"
    );
    package_manager
        || contains_any(
            output,
            &[
                "unable to resolve dependency tree",
                "no matching distribution found",
                "could not find a version that satisfies",
                "formula unavailable",
                "package not found",
                "err_pnpm",
            ],
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn input(command: &str, exit_code: i32, stderr: &str) -> AnalysisInput {
        let mut input = AnalysisInput::new(command, exit_code, Path::new("/tmp"));
        input.stderr = stderr.to_owned();
        input
    }

    #[test]
    fn command_not_found_with_spelling_suggestion() {
        let result =
            LocalAnalyzer::new().analyze(&input("treee", 127, "zsh: command not found: treee"));
        assert_eq!(result.category, AnalysisCategory::CommandNotFound);
        assert_eq!(result.suggested_command.as_deref(), Some("tree"));
        assert!(result.needs_ai);
    }

    #[test]
    fn detects_permission_and_file_errors() {
        let analyzer = LocalAnalyzer::new();
        assert_eq!(
            analyzer
                .analyze(&input("cat /root/a", 1, "Permission denied"))
                .category,
            AnalysisCategory::PermissionDenied
        );
        assert_eq!(
            analyzer
                .analyze(&input("cat config.tx", 1, "No such file or directory"))
                .category,
            AnalysisCategory::FileNotFound
        );
    }

    #[test]
    fn detects_git_typo_without_ai() {
        let result = LocalAnalyzer::new().analyze(&input(
            "git pul origin main",
            1,
            "git: 'pul' is not a git command",
        ));
        assert_eq!(result.category, AnalysisCategory::Git);
        assert_eq!(
            result.suggested_command.as_deref(),
            Some("git pull origin main")
        );
        assert!(!result.needs_ai);
    }

    #[test]
    fn detects_docker_compose_typo() {
        let result =
            LocalAnalyzer::new().analyze(&input("docker compose upp", 1, "unknown command"));
        assert_eq!(result.category, AnalysisCategory::Docker);
        assert_eq!(
            result.suggested_command.as_deref(),
            Some("docker compose up")
        );
    }

    #[test]
    fn detects_network_compiler_ssh_and_package_failures() {
        let analyzer = LocalAnalyzer::new();
        let cases = [
            (
                "curl https://bad.invalid",
                "curl: (6) Could not resolve host: bad.invalid",
                AnalysisCategory::Network,
            ),
            (
                "cargo build",
                "error[E0308]: mismatched types\nerror: could not compile demo",
                AnalysisCategory::Compiler,
            ),
            (
                "ssh host",
                "Permission denied (publickey,password).",
                AnalysisCategory::Ssh,
            ),
            (
                "git pull",
                "git@example.test: Permission denied (publickey).",
                AnalysisCategory::Ssh,
            ),
            (
                "npm install",
                "npm ERR! unable to resolve dependency tree",
                AnalysisCategory::PackageManager,
            ),
        ];
        for (command, output, category) in cases {
            assert_eq!(
                analyzer.analyze(&input(command, 1, output)).category,
                category,
                "{command}"
            );
        }
    }

    #[test]
    fn normal_successes_stay_silent_even_if_output_contains_error_words() {
        let analyzer = LocalAnalyzer::new();
        for command in ["ls", "pwd", "cd /tmp", "clear", "git status"] {
            assert!(!analyzer.analyze(&input(command, 0, "")).needs_response());
        }
        assert!(
            !analyzer
                .analyze(&input("echo permission denied", 0, "permission denied"))
                .needs_response()
        );
    }

    #[test]
    fn dangerous_input_is_found_before_execution() {
        let result = LocalAnalyzer::new().analyze(&input("rm -rf /", 0, ""));
        assert_eq!(result.category, AnalysisCategory::DangerousCommand);
        assert_eq!(result.severity, Severity::Critical);
        assert!(!result.needs_ai);
    }

    #[test]
    fn transposition_distance_supports_obvious_typos() {
        assert_eq!(damerau_levenshtein("gti", "git"), 1);
        assert_eq!(closest_word("gti", COMMON_COMMANDS), Some("git"));
    }
}
