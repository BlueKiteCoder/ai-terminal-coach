use std::cmp::Reverse;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::SafetyConfig;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskLevel {
    #[default]
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SafetyFinding {
    pub rule_id: String,
    pub level: RiskLevel,
    pub message: String,
    /// The smallest shell command segment associated with this finding.
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SafetyAssessment {
    pub level: RiskLevel,
    pub findings: Vec<SafetyFinding>,
}

impl SafetyAssessment {
    pub fn is_dangerous(&self) -> bool {
        self.level >= RiskLevel::Medium
    }

    pub fn requires_warning(&self) -> bool {
        self.is_dangerous()
    }

    pub fn primary_finding(&self) -> Option<&SafetyFinding> {
        self.findings.iter().max_by_key(|finding| finding.level)
    }

    fn add(&mut self, rule_id: &str, level: RiskLevel, message: &str, command: &str) {
        if self
            .findings
            .iter()
            .any(|finding| finding.rule_id == rule_id && finding.command == command)
        {
            return;
        }
        self.level = self.level.max(level);
        self.findings.push(SafetyFinding {
            rule_id: rule_id.to_owned(),
            level,
            message: message.to_owned(),
            command: command.trim().to_owned(),
        });
    }
}

#[derive(Debug, Clone)]
pub struct SafetyEngine {
    config: SafetyConfig,
}

impl Default for SafetyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SafetyEngine {
    pub fn new() -> Self {
        Self {
            config: SafetyConfig::default(),
        }
    }

    pub fn with_config(config: SafetyConfig) -> Self {
        Self { config }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn assess(&self, source: &str) -> SafetyAssessment {
        if !self.config.enabled || source.trim().is_empty() {
            return SafetyAssessment::default();
        }

        let source = remove_heredoc_bodies(source);
        let mut assessment = SafetyAssessment::default();
        self.assess_source(&source, 0, &mut assessment);
        assessment
            .findings
            .sort_by_key(|finding| Reverse(finding.level));
        assessment
    }

    pub fn risk_level(&self, source: &str) -> RiskLevel {
        self.assess(source).level
    }

    fn assess_source(&self, source: &str, depth: usize, assessment: &mut SafetyAssessment) {
        if depth > 4 {
            return;
        }

        let compact: String = unquoted_shell_text(source)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        if compact.contains(":(){:|:&};:") {
            assessment.add(
                "shell.fork-bomb",
                RiskLevel::Critical,
                "This is a fork bomb and can immediately exhaust system resources.",
                source,
            );
        }

        if has_download_to_shell_pipeline(source) {
            assessment.add(
                "shell.download-pipe",
                RiskLevel::High,
                "Downloaded code is piped directly into a shell without inspection.",
                source,
            );
        }

        for substitution in command_substitutions(source) {
            self.assess_source(&substitution, depth + 1, assessment);
        }

        for segment in split_shell_commands(source) {
            self.assess_segment(&segment, depth, assessment);
        }
    }

    fn assess_segment(&self, segment: &str, depth: usize, assessment: &mut SafetyAssessment) {
        let tokens = match shell_words::split(segment) {
            Ok(tokens) => tokens,
            Err(_) => lenient_tokens(segment),
        };
        if tokens.is_empty() {
            return;
        }
        let Some(command_index) = command_index(&tokens) else {
            return;
        };
        let command = executable_name(&tokens[command_index]);
        let args = &tokens[command_index + 1..];

        // Commands whose payload is another program need a second parse.  A
        // quoted `sh -c 'rm -rf /'` is executable code, unlike `echo 'rm -rf /'`.
        if matches!(command.as_str(), "sh" | "bash" | "zsh") {
            if let Some(payload) = argument_after(args, "-c") {
                self.assess_source(payload, depth + 1, assessment);
            }
        } else if command == "eval" {
            if !args.is_empty() {
                self.assess_source(&args.join(" "), depth + 1, assessment);
            }
        } else if command == "xargs"
            && let Some(nested) = xargs_command(args)
        {
            self.assess_segment(&nested, depth + 1, assessment);
        }

        match command.as_str() {
            "rm" => assess_rm(
                segment,
                args,
                tokens[..command_index]
                    .iter()
                    .any(|token| executable_name(token) == "sudo"),
                assessment,
            ),
            command if command == "mkfs" || command.starts_with("mkfs.") => {
                if !has_help_flag(args) {
                    assessment.add(
                        "disk.mkfs",
                        RiskLevel::Critical,
                        "Formatting a filesystem destroys existing data on the target device.",
                        segment,
                    );
                }
            }
            "dd" => assess_dd(segment, args, assessment),
            "diskutil" => assess_diskutil(segment, args, assessment),
            "git" => assess_git(segment, args, assessment),
            "chmod" => assess_chmod(segment, args, assessment),
            "chown" => assess_chown(segment, args, assessment),
            "kill" | "pkill" | "killall" => assess_kill(segment, &command, args, assessment),
            "find" => assess_find(segment, args, assessment),
            "shutdown" | "reboot" | "halt" => {
                if !has_help_flag(args) {
                    assessment.add(
                        "system.power",
                        RiskLevel::High,
                        "This command stops or restarts the computer and active processes.",
                        segment,
                    );
                }
            }
            "truncate" => {
                if args
                    .windows(2)
                    .any(|pair| pair[0] == "-s" && pair[1] == "0")
                {
                    assessment.add(
                        "file.truncate",
                        RiskLevel::High,
                        "This command irreversibly truncates a file to zero bytes.",
                        segment,
                    );
                }
            }
            "drop" => assess_sql(segment, &tokens[command_index..], assessment),
            "mysql" | "psql" | "sqlite3" => {
                if let Some(sql) = sql_argument(args) {
                    assess_sql(segment, &lenient_tokens(sql), assessment);
                }
            }
            _ => {}
        }

        assess_raw_device_redirect(segment, &tokens, assessment);
    }
}

fn assess_rm(segment: &str, args: &[String], sudo: bool, assessment: &mut SafetyAssessment) {
    if has_help_flag(args) {
        return;
    }
    let mut recursive = false;
    let mut force = false;
    let mut targets = Vec::new();
    let mut options_done = false;
    for argument in args {
        if !options_done && argument == "--" {
            options_done = true;
        } else if !options_done && argument.starts_with('-') {
            recursive |= argument[1..].chars().any(|flag| matches!(flag, 'r' | 'R'));
            force |= argument[1..].chars().any(|flag| flag == 'f');
        } else {
            targets.push(argument.as_str());
        }
    }
    if !recursive || targets.is_empty() {
        return;
    }

    for target in targets {
        let normalized = target.trim_end_matches('/');
        let home = (normalized == "~" && !is_shell_quoted_literal(segment, target))
            || (matches!(normalized, "$HOME" | "${HOME}")
                && !is_single_quoted_literal(segment, target));
        let root = normalized.is_empty()
            || matches!(
                normalized,
                "/" | "/*" | "/System" | "/usr" | "/etc" | "/var" | "/Users"
            );
        if root || home {
            assessment.add(
                "rm.protected-root",
                RiskLevel::Critical,
                "Recursive removal targets the filesystem root, a system tree, or the whole home directory.",
                segment,
            );
            continue;
        }
        if target.starts_with("~/")
            || target.starts_with("$HOME/")
            || target.starts_with("${HOME}/")
        {
            assessment.add(
                "rm.home-subtree",
                RiskLevel::High,
                "Recursive removal targets content inside the home directory.",
                segment,
            );
            continue;
        }
        let has_active_glob = (target.contains('*') || target.contains('?'))
            && !is_shell_quoted_literal(segment, target);
        if has_active_glob {
            assessment.add(
                "rm.recursive-glob",
                RiskLevel::High,
                "Recursive removal uses a glob that may match more files than intended.",
                segment,
            );
            continue;
        }

        let level = if sudo {
            RiskLevel::High
        } else {
            RiskLevel::Medium
        };
        let message = if force {
            "Recursive forced removal permanently deletes a directory tree."
        } else {
            "Recursive removal permanently deletes a directory tree."
        };
        assessment.add("rm.recursive", level, message, segment);
    }
}

fn assess_dd(segment: &str, args: &[String], assessment: &mut SafetyAssessment) {
    if has_help_flag(args) {
        return;
    }
    let output = args
        .iter()
        .find_map(|argument| argument.strip_prefix("of="));
    let Some(output) = output else {
        return;
    };
    let raw_device = is_raw_device(output);
    assessment.add(
        if raw_device {
            "disk.dd-device"
        } else {
            "file.dd-overwrite"
        },
        if raw_device {
            RiskLevel::Critical
        } else {
            RiskLevel::High
        },
        if raw_device {
            "`dd` writes directly to a disk device and can destroy its partition table and data."
        } else {
            "`dd` overwrites its output file without an interactive confirmation."
        },
        segment,
    );
}

fn assess_diskutil(segment: &str, args: &[String], assessment: &mut SafetyAssessment) {
    if has_help_flag(args) {
        return;
    }
    let subcommand = args.first().map(|value| value.to_ascii_lowercase());
    match subcommand.as_deref() {
        Some("erasedisk" | "partitiondisk" | "zerodisk" | "randomdisk") => assessment.add(
            "disk.diskutil-erase",
            RiskLevel::Critical,
            "This diskutil operation destroys data across an entire disk.",
            segment,
        ),
        Some("erasevolume" | "apfs")
            if args
                .iter()
                .any(|arg| arg.eq_ignore_ascii_case("deletevolume")) =>
        {
            assessment.add(
                "disk.diskutil-volume",
                RiskLevel::High,
                "This diskutil operation erases or deletes a volume.",
                segment,
            );
        }
        Some("erasevolume") => assessment.add(
            "disk.diskutil-volume",
            RiskLevel::High,
            "This diskutil operation erases a volume.",
            segment,
        ),
        _ => {}
    }
}

fn assess_git(segment: &str, args: &[String], assessment: &mut SafetyAssessment) {
    let Some(subcommand_index) = args.iter().position(|argument| !argument.starts_with('-')) else {
        return;
    };
    let subcommand = args[subcommand_index].as_str();
    let remaining = &args[subcommand_index + 1..];
    match subcommand {
        "reset" if remaining.iter().any(|argument| argument == "--hard") => assessment.add(
            "git.reset-hard",
            RiskLevel::High,
            "`git reset --hard` discards uncommitted tracked-file changes.",
            segment,
        ),
        "clean" => {
            let dry_run = remaining.iter().any(|argument| {
                argument == "--dry-run"
                    || (argument.starts_with('-')
                        && !argument.starts_with("--")
                        && argument[1..].contains('n'))
            });
            let force = remaining.iter().any(|argument| {
                argument == "--force" || (argument.starts_with('-') && argument[1..].contains('f'))
            });
            if force && !dry_run {
                assessment.add(
                    "git.clean-force",
                    RiskLevel::High,
                    "Forced git clean permanently deletes untracked files (and possibly directories).",
                    segment,
                );
            }
        }
        "push" => {
            if remaining.iter().any(|argument| {
                argument == "--force"
                    || (argument.starts_with('-')
                        && !argument.starts_with("--")
                        && argument[1..].contains('f'))
            }) {
                assessment.add(
                    "git.push-force",
                    RiskLevel::High,
                    "Force-pushing can overwrite commits on the remote branch.",
                    segment,
                );
            } else if remaining
                .iter()
                .any(|argument| argument == "--force-with-lease")
            {
                assessment.add(
                    "git.push-force-with-lease",
                    RiskLevel::Medium,
                    "Force-with-lease rewrites remote history, though it checks for unexpected updates.",
                    segment,
                );
            }
        }
        _ => {}
    }
}

fn assess_chmod(segment: &str, args: &[String], assessment: &mut SafetyAssessment) {
    let recursive = args
        .iter()
        .any(|argument| argument == "-R" || argument == "--recursive");
    let world_writable = args.iter().any(|argument| {
        let normalized = argument.trim_start_matches('0');
        normalized == "777" || argument.eq_ignore_ascii_case("a+rwx")
    });
    if world_writable {
        assessment.add(
            "permissions.world-writable",
            if recursive {
                RiskLevel::High
            } else {
                RiskLevel::Medium
            },
            "World-writable permissions let every local user modify the target.",
            segment,
        );
    }
}

fn assess_chown(segment: &str, args: &[String], assessment: &mut SafetyAssessment) {
    let recursive = args
        .iter()
        .any(|argument| argument == "-R" || argument == "--recursive");
    if !recursive {
        return;
    }
    let root_target = args.iter().any(|argument| {
        let normalized = argument.trim_end_matches('/');
        normalized.is_empty() || matches!(normalized, "/" | "/System" | "/usr" | "/Users")
    });
    assessment.add(
        "permissions.recursive-owner",
        if root_target {
            RiskLevel::Critical
        } else {
            RiskLevel::High
        },
        "Recursive ownership changes can make files inaccessible or break the operating system.",
        segment,
    );
}

fn assess_kill(segment: &str, command: &str, args: &[String], assessment: &mut SafetyAssessment) {
    let sigkill = args.iter().any(|argument| {
        matches!(
            argument.to_ascii_uppercase().as_str(),
            "-9" | "-KILL" | "--SIGNAL=KILL"
        )
    }) || args.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-s" | "--signal") && pair[1].eq_ignore_ascii_case("KILL")
    });
    if !sigkill {
        return;
    }
    let broad = command == "killall"
        || args
            .iter()
            .any(|argument| matches!(argument.as_str(), "-1" | "0" | "1"));
    assessment.add(
        "process.sigkill",
        if broad {
            RiskLevel::Critical
        } else {
            RiskLevel::High
        },
        "SIGKILL prevents the target process from saving state or cleaning up.",
        segment,
    );
}

fn assess_find(segment: &str, args: &[String], assessment: &mut SafetyAssessment) {
    if !args.iter().any(|argument| argument == "-delete") {
        return;
    }
    let root = args
        .first()
        .is_some_and(|argument| argument == "/" || argument == "~" || argument == "$HOME");
    assessment.add(
        "find.delete",
        if root {
            RiskLevel::Critical
        } else {
            RiskLevel::High
        },
        "`find -delete` permanently removes every matching path.",
        segment,
    );
}

fn assess_sql(segment: &str, tokens: &[String], assessment: &mut SafetyAssessment) {
    let lowercase: Vec<String> = tokens
        .iter()
        .map(|token| token.to_ascii_lowercase())
        .collect();
    for window in lowercase.windows(2) {
        match window {
            [drop, object] if drop == "drop" && object == "database" => assessment.add(
                "sql.drop-database",
                RiskLevel::Critical,
                "DROP DATABASE irreversibly removes a database and all of its objects.",
                segment,
            ),
            [drop, object] if drop == "drop" && object == "table" => assessment.add(
                "sql.drop-table",
                RiskLevel::High,
                "DROP TABLE removes the table definition and its stored data.",
                segment,
            ),
            _ => {}
        }
    }
}

fn assess_raw_device_redirect(segment: &str, tokens: &[String], assessment: &mut SafetyAssessment) {
    let separated_redirect = tokens.windows(2).any(|pair| {
        matches!(pair[0].as_str(), ">" | ">>" | "1>" | "2>") && is_raw_device(&pair[1])
    });
    let unquoted = unquoted_shell_text(segment);
    let attached_redirect = [">/dev/disk", ">/dev/rdisk", ">>/dev/disk", ">>/dev/rdisk"]
        .iter()
        .any(|redirect| unquoted.contains(redirect));
    if separated_redirect || attached_redirect {
        assessment.add(
            "disk.raw-redirect",
            RiskLevel::Critical,
            "Shell output is redirected directly to a raw disk device.",
            segment,
        );
    }
}

pub(crate) fn command_index(tokens: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let command = executable_name(token);
        if token.contains('=') && !token.starts_with('=') && !token.starts_with('/') {
            index += 1;
            continue;
        }
        if matches!(command.as_str(), "command" | "builtin" | "nohup") {
            index += 1;
            while index < tokens.len() && tokens[index].starts_with('-') {
                index += 1;
            }
            continue;
        }
        if command == "sudo" {
            index += 1;
            while index < tokens.len() {
                let argument = &tokens[index];
                if argument == "-u" || argument == "-g" || argument == "-h" || argument == "-p" {
                    index += 2;
                } else if argument.starts_with('-') {
                    index += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        if command == "env" {
            index += 1;
            while index < tokens.len()
                && (tokens[index].starts_with('-') || tokens[index].contains('='))
            {
                index += 1;
            }
            continue;
        }
        return Some(index);
    }
    None
}

pub(crate) fn executable_name(token: &str) -> String {
    token
        .rsplit('/')
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase()
}

fn has_help_flag(args: &[String]) -> bool {
    args.iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help" | "--version"))
}

fn argument_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
}

fn sql_argument(args: &[String]) -> Option<&str> {
    ["-e", "-c", "--execute", "--command"]
        .iter()
        .find_map(|flag| argument_after(args, flag))
}

fn xargs_command(args: &[String]) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if matches!(argument.as_str(), "-I" | "-L" | "-n" | "-P" | "-s") {
            index += 2;
        } else if argument.starts_with('-') {
            index += 1;
        } else {
            return Some(args[index..].join(" "));
        }
    }
    None
}

fn is_raw_device(path: &str) -> bool {
    path.starts_with("/dev/disk") || path.starts_with("/dev/rdisk")
}

fn is_shell_quoted_literal(segment: &str, value: &str) -> bool {
    segment.contains(&format!("'{value}'")) || segment.contains(&format!("\"{value}\""))
}

fn is_single_quoted_literal(segment: &str, value: &str) -> bool {
    segment.contains(&format!("'{value}'"))
}

/// Split only at unquoted shell control operators.  Keeping the original text
/// lets findings point at the exact command and avoids interpreting quoted
/// examples such as `echo "rm -rf /"` as executable commands.
pub(crate) fn split_shell_commands(source: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if comment {
            if character == '\n' {
                comment = false;
                push_segment(&mut commands, &mut current);
            }
            index += 1;
            continue;
        }
        if escaped {
            current.push(character);
            escaped = false;
            index += 1;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            current.push(character);
            escaped = true;
            index += 1;
            continue;
        }
        if let Some(active_quote) = quote {
            current.push(character);
            if character == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            current.push(character);
            index += 1;
            continue;
        }
        if character == '#' && current.chars().next_back().is_none_or(char::is_whitespace) {
            comment = true;
            index += 1;
            continue;
        }
        if matches!(character, ';' | '\n' | '|') || character == '&' {
            push_segment(&mut commands, &mut current);
            // Consume the second half of && and ||.
            if index + 1 < chars.len() && chars[index + 1] == character {
                index += 1;
            }
            index += 1;
            continue;
        }
        current.push(character);
        index += 1;
    }
    push_segment(&mut commands, &mut current);
    commands
}

fn push_segment(commands: &mut Vec<String>, current: &mut String) {
    let segment = current.trim();
    if !segment.is_empty() {
        commands.push(segment.to_owned());
    }
    current.clear();
}

pub(crate) fn lenient_tokens(source: &str) -> Vec<String> {
    source
        .split_whitespace()
        .map(|token| token.trim_matches(['\'', '"']).to_owned())
        .collect()
}

fn has_download_to_shell_pipeline(source: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let chars: Vec<char> = source.chars().collect();
    for (index, character) in chars.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            continue;
        }
        if character != '|' || chars.get(index + 1) == Some(&'|') {
            continue;
        }
        let left: String = chars[..index].iter().collect();
        let right: String = chars[index + 1..].iter().collect();
        let left_segment = left.rsplit([';', '&', '\n', '|']).next().unwrap_or("");
        let right_segment = right.split([';', '&', '\n', '|']).next().unwrap_or("");
        let left_tokens = shell_words::split(left_segment).unwrap_or_default();
        let right_tokens = shell_words::split(right_segment).unwrap_or_default();
        let left_command =
            command_index(&left_tokens).map(|position| executable_name(&left_tokens[position]));
        let right_command =
            command_index(&right_tokens).map(|position| executable_name(&right_tokens[position]));
        if matches!(left_command.as_deref(), Some("curl" | "wget"))
            && matches!(right_command.as_deref(), Some("sh" | "bash" | "zsh"))
        {
            return true;
        }
    }
    false
}

fn command_substitutions(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut substitutions = Vec::new();
    let mut single_quote = false;
    let mut double_quote = false;
    let mut comment = false;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if comment {
            if character == '\n' {
                comment = false;
            }
            index += 1;
            continue;
        }
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if character == '\\' && !single_quote {
            escaped = true;
            index += 1;
            continue;
        }
        if character == '\'' && !double_quote {
            single_quote = !single_quote;
            index += 1;
            continue;
        }
        if character == '"' && !single_quote {
            double_quote = !double_quote;
            index += 1;
            continue;
        }
        if single_quote {
            index += 1;
            continue;
        }
        if !double_quote && character == '#' && (index == 0 || chars[index - 1].is_whitespace()) {
            comment = true;
            index += 1;
            continue;
        }
        if character == '$' && chars.get(index + 1) == Some(&'(') {
            let start = index + 2;
            let mut depth = 1;
            let mut cursor = start;
            while cursor < chars.len() {
                if chars[cursor] == '(' {
                    depth += 1;
                } else if chars[cursor] == ')' {
                    depth -= 1;
                    if depth == 0 {
                        substitutions.push(chars[start..cursor].iter().collect());
                        index = cursor;
                        break;
                    }
                }
                cursor += 1;
            }
        } else if character == '`' {
            let start = index + 1;
            if let Some(offset) = chars[start..].iter().position(|value| *value == '`') {
                let end = start + offset;
                substitutions.push(chars[start..end].iter().collect());
                index = end;
            }
        }
        index += 1;
    }
    substitutions
}

/// Return executable shell syntax while omitting quoted literal contents and
/// comments. Shell `-c` payloads are inspected separately after tokenization.
fn unquoted_shell_text(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut previous = None;
    for character in source.chars() {
        if comment {
            if character == '\n' {
                comment = false;
                output.push(character);
            }
            previous = Some(character);
            continue;
        }
        if escaped {
            // Escaped characters are literal, so preserve spacing but not the
            // potentially misleading command text itself.
            output.push(' ');
            escaped = false;
            previous = Some(character);
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            output.push(' ');
            previous = Some(character);
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            output.push(if character == '\n' { '\n' } else { ' ' });
            previous = Some(character);
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            output.push(' ');
            previous = Some(character);
            continue;
        }
        if character == '#' && previous.is_none_or(char::is_whitespace) {
            comment = true;
            output.push(' ');
            previous = Some(character);
            continue;
        }
        output.push(character);
        previous = Some(character);
    }
    output
}

fn remove_heredoc_bodies(source: &str) -> String {
    let mut result = String::new();
    let mut delimiter: Option<String> = None;
    for line in source.lines() {
        if let Some(active) = &delimiter {
            if line.trim() == active {
                delimiter = None;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
        delimiter = heredoc_delimiter(line);
    }
    result
}

fn heredoc_delimiter(line: &str) -> Option<String> {
    let marker = line.find("<<")?;
    let before = &line[..marker];
    // A quoted prose example is not a shell here-document operator.
    if before.matches('\'').count() % 2 == 1 || before.matches('"').count() % 2 == 1 {
        return None;
    }
    let mut rest = line[marker + 2..].trim_start();
    rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    let token = rest.split_whitespace().next()?;
    let delimiter = token.trim_matches(['\'', '"']);
    (!delimiter.is_empty()
        && delimiter
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then(|| delimiter.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risk(command: &str) -> RiskLevel {
        SafetyEngine::new().risk_level(command)
    }

    #[test]
    fn required_destructive_commands_have_expected_risk() {
        let cases = [
            ("rm -rf /", RiskLevel::Critical),
            ("rm -rf ~", RiskLevel::Critical),
            ("rm -rf *", RiskLevel::High),
            ("sudo rm -rf /tmp/build", RiskLevel::High),
            ("mkfs.apfs /dev/disk4", RiskLevel::Critical),
            ("dd if=/dev/zero of=/dev/rdisk2", RiskLevel::Critical),
            (
                "diskutil eraseDisk APFS Empty /dev/disk4",
                RiskLevel::Critical,
            ),
            ("git reset --hard HEAD", RiskLevel::High),
            ("git clean -fd", RiskLevel::High),
            ("DROP DATABASE production", RiskLevel::Critical),
            ("DROP TABLE users", RiskLevel::High),
            ("chmod -R 777 .", RiskLevel::High),
            ("kill -9 123", RiskLevel::High),
        ];
        for (command, expected) in cases {
            assert_eq!(risk(command), expected, "{command}");
        }
    }

    #[test]
    fn ordinary_and_dry_run_commands_are_low_risk() {
        for command in [
            "ls -la",
            "git status",
            "git reset --soft HEAD~1",
            "git clean -nd",
            "git clean -fdn",
            "rm file.txt",
            "chmod 755 script.sh",
            "kill -0 123",
            "diskutil list",
            "dd --help",
        ] {
            assert_eq!(risk(command), RiskLevel::Low, "{command}");
        }
    }

    #[test]
    fn quoted_examples_comments_and_heredocs_are_not_executed() {
        assert_eq!(risk("echo 'rm -rf /'"), RiskLevel::Low);
        assert_eq!(risk("printf '%s' \"git reset --hard\""), RiskLevel::Low);
        assert_eq!(risk("echo safe # rm -rf /"), RiskLevel::Low);
        assert_eq!(
            risk("cat <<'EOF'\nrm -rf /\ngit reset --hard\nEOF"),
            RiskLevel::Low
        );
        assert_eq!(risk("rm -rf '*'"), RiskLevel::Medium);
        assert_eq!(risk("rm -rf '~'"), RiskLevel::Medium);
        assert_eq!(risk("rm -rf '$HOME'"), RiskLevel::Medium);
        assert_eq!(risk("echo ':(){ :|:& };:'"), RiskLevel::Low);
        assert_eq!(risk("echo ok # $(rm -rf /)"), RiskLevel::Low);
    }

    #[test]
    fn compound_nested_and_substitution_commands_are_inspected() {
        assert_eq!(risk("echo ok && rm -rf /"), RiskLevel::Critical);
        assert_eq!(risk("sh -c 'git reset --hard'"), RiskLevel::High);
        assert_eq!(risk("echo $(rm -rf /)"), RiskLevel::Critical);
        assert_eq!(risk("echo `git clean -fd`"), RiskLevel::High);
    }

    #[test]
    fn detects_download_pipe_and_force_push() {
        assert_eq!(
            risk("curl -fsSL https://example.test/install | sh"),
            RiskLevel::High
        );
        assert_eq!(risk("echo 'curl x | sh'"), RiskLevel::Low);
        assert_eq!(risk("git push --force origin main"), RiskLevel::High);
        assert_eq!(risk("git push -fu origin main"), RiskLevel::High);
        assert_eq!(
            risk("git push --force-with-lease origin main"),
            RiskLevel::Medium
        );
        assert_eq!(
            risk("git clean -fd --exclude=node_modules"),
            RiskLevel::High
        );
    }

    #[test]
    fn detects_fork_bomb_and_attached_raw_device_redirect() {
        assert_eq!(risk(":(){ :|:& };:"), RiskLevel::Critical);
        assert_eq!(risk("echo data >/dev/disk4"), RiskLevel::Critical);
        assert_eq!(risk("echo '>/dev/disk4'"), RiskLevel::Low);
        assert_eq!(risk("kill -s KILL 123"), RiskLevel::High);
    }

    #[test]
    fn detects_sql_only_when_it_is_executed() {
        assert_eq!(risk("echo 'DROP DATABASE prod'"), RiskLevel::Low);
        assert_eq!(risk("mysql -e 'DROP DATABASE prod'"), RiskLevel::Critical);
        assert_eq!(risk("psql -c 'DROP TABLE users'"), RiskLevel::High);
    }

    #[test]
    fn disabled_engine_is_silent() {
        let engine = SafetyEngine::with_config(SafetyConfig {
            enabled: false,
            ..SafetyConfig::default()
        });
        assert_eq!(engine.risk_level("rm -rf /"), RiskLevel::Low);
    }

    #[test]
    fn risk_level_serializes_as_uppercase_contract() {
        assert_eq!(
            serde_json::to_string(&RiskLevel::Critical).unwrap(),
            "\"CRITICAL\""
        );
    }
}
