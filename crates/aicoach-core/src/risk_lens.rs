//! Honest, local impact summaries for commands that have not been executed.

use serde::{Deserialize, Serialize};

use crate::{
    RiskLevel, SafetyEngine, SafetyFinding,
    safety::{command_index, executable_name, lenient_tokens, split_shell_commands},
};

const MAX_RISK_LENS_CHARS: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectAction {
    NoneDetected,
    Read,
    Create,
    Modify,
    Delete,
    Execute,
    Network,
    Process,
    System,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskEffect {
    pub action: EffectAction,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Variants are ordered from least to most privilege required.
pub enum PrivilegeRequirement {
    CurrentUser,
    Unknown,
    ElevatedLikely,
    Administrator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Variants are ordered by how cautiously the combined report should be read.
pub enum RecoveryProspect {
    NotApplicable,
    Reversible,
    Limited,
    Unknown,
    Irreversible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisCoverage {
    Recognized,
    Partial,
    Unknown,
}

/// A local-only description of a command's likely side effects.
///
/// `level = None` deliberately means "unrated", not low risk. This prevents an
/// unfamiliar executable from receiving a reassuring verdict merely because
/// no destructive rule recognized it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskLensReport {
    pub level: Option<RiskLevel>,
    pub effects: Vec<RiskEffect>,
    pub privilege: PrivilegeRequirement,
    pub recovery: RecoveryProspect,
    pub coverage: AnalysisCoverage,
    pub safety_rules_enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_ids: Vec<String>,
}

#[derive(Debug)]
struct CommandProfile {
    effect: RiskEffect,
    level: RiskLevel,
    recovery: RecoveryProspect,
    privilege: PrivilegeRequirement,
}

struct LensAggregate {
    effects: Vec<RiskEffect>,
    level: Option<RiskLevel>,
    privilege: PrivilegeRequirement,
    recovery: RecoveryProspect,
    recognized: usize,
    unknown: usize,
}

impl LensAggregate {
    fn inspect(source: &str, level: Option<RiskLevel>) -> Self {
        let mut aggregate = Self {
            effects: Vec::new(),
            level,
            privilege: PrivilegeRequirement::CurrentUser,
            recovery: RecoveryProspect::NotApplicable,
            recognized: 0,
            unknown: 0,
        };
        for segment in split_shell_commands(source) {
            aggregate.inspect_segment(&segment);
        }
        aggregate
    }

    fn inspect_segment(&mut self, segment: &str) {
        let tokens = shell_words::split(segment).unwrap_or_else(|_| lenient_tokens(segment));
        let Some(executable_index) = command_index(&tokens) else {
            if let Some(target) = redirected_target(segment) {
                self.add_profile(simple_profile(
                    EffectAction::Modify,
                    target,
                    RiskLevel::Medium,
                    RecoveryProspect::Limited,
                ));
            } else {
                self.unknown += 1;
            }
            return;
        };
        let executable = executable_name(&tokens[executable_index]);
        let args = &tokens[executable_index + 1..];
        if tokens[..executable_index]
            .iter()
            .any(|token| executable_name(token) == "sudo")
        {
            self.privilege = self.privilege.max(PrivilegeRequirement::Administrator);
        }
        if let Some(profile) = profile_command(&executable, args, segment) {
            self.add_profile(profile);
        } else {
            self.unknown += 1;
        }
        if let Some(target) = redirected_target(segment) {
            self.add_profile(simple_profile(
                EffectAction::Modify,
                target,
                RiskLevel::Medium,
                RecoveryProspect::Limited,
            ));
        }
    }

    fn add_profile(&mut self, profile: CommandProfile) {
        self.recognized += 1;
        self.level = Some(
            self.level
                .map_or(profile.level, |current| current.max(profile.level)),
        );
        self.recovery = self.recovery.max(profile.recovery);
        self.privilege = self.privilege.max(profile.privilege);
        push_effect(&mut self.effects, profile.effect);
    }
}

impl SafetyEngine {
    /// Inspect likely command effects without executing the command or using a
    /// network provider.
    pub fn risk_lens(&self, source: &str) -> RiskLensReport {
        if source.chars().take(MAX_RISK_LENS_CHARS + 1).count() > MAX_RISK_LENS_CHARS {
            return oversized_report(self.is_enabled());
        }
        let assessment = self.assess(source);
        let initial_level = self
            .is_enabled()
            .then_some(assessment.level)
            .filter(|level| *level > RiskLevel::Low);
        let mut lens = LensAggregate::inspect(source, initial_level);

        for finding in &assessment.findings {
            if let Some(effect) = effect_from_finding(finding)
                && !lens
                    .effects
                    .iter()
                    .any(|existing| existing.action == effect.action)
            {
                push_effect(&mut lens.effects, effect);
            }
            lens.recovery = lens.recovery.max(recovery_from_rule(&finding.rule_id));
            if finding.rule_id.starts_with("disk.") {
                lens.privilege = lens.privilege.max(PrivilegeRequirement::ElevatedLikely);
            }
        }
        lens.recognized += assessment.findings.len();

        let coverage = match (lens.recognized, lens.unknown) {
            (0, _) => AnalysisCoverage::Unknown,
            (_, 0) => AnalysisCoverage::Recognized,
            _ => AnalysisCoverage::Partial,
        };
        if lens.effects.is_empty() {
            lens.effects.push(RiskEffect {
                action: if source.trim().is_empty() {
                    EffectAction::NoneDetected
                } else {
                    EffectAction::Unknown
                },
                target: if source.trim().is_empty() {
                    "empty command buffer".to_owned()
                } else {
                    "effects outside the local rule set".to_owned()
                },
            });
        }
        if matches!(coverage, AnalysisCoverage::Unknown) {
            lens.level = None;
            if lens.privilege == PrivilegeRequirement::CurrentUser {
                lens.privilege = PrivilegeRequirement::Unknown;
            }
            lens.recovery = RecoveryProspect::Unknown;
        } else if lens.unknown > 0 {
            if lens.level == Some(RiskLevel::Low) {
                lens.level = None;
            }
            if lens.privilege == PrivilegeRequirement::CurrentUser {
                lens.privilege = PrivilegeRequirement::Unknown;
            }
            lens.recovery = lens.recovery.max(RecoveryProspect::Unknown);
        }

        let mut rule_ids = Vec::new();
        for finding in assessment.findings {
            if !rule_ids.contains(&finding.rule_id) {
                rule_ids.push(finding.rule_id);
            }
        }

        RiskLensReport {
            level: lens.level,
            effects: lens.effects,
            privilege: lens.privilege,
            recovery: lens.recovery,
            coverage,
            safety_rules_enabled: self.is_enabled(),
            rule_ids,
        }
    }
}

fn oversized_report(safety_rules_enabled: bool) -> RiskLensReport {
    RiskLensReport {
        level: None,
        effects: vec![RiskEffect {
            action: EffectAction::Unknown,
            target: "command exceeds the local analysis limit".to_owned(),
        }],
        privilege: PrivilegeRequirement::Unknown,
        recovery: RecoveryProspect::Unknown,
        coverage: AnalysisCoverage::Unknown,
        safety_rules_enabled,
        rule_ids: Vec::new(),
    }
}

fn profile_command(executable: &str, args: &[String], segment: &str) -> Option<CommandProfile> {
    profile_file_or_process_command(executable, args, segment).or_else(|| match executable {
        "defaults" => profile_defaults(args),
        "diskutil" => profile_diskutil(args),
        "git" => profile_git(args),
        "brew" => profile_package_manager("Homebrew packages", args),
        "npm" | "pnpm" | "yarn" => profile_package_manager("JavaScript dependencies", args),
        "pip" | "pip3" | "uv" => profile_package_manager("Python environment", args),
        "docker" => profile_docker(args),
        "kubectl" => profile_kubectl(args),
        "launchctl" => profile_launchctl(args),
        "echo" | "printf" => redirected_target(segment).map(|target| {
            simple_profile(
                EffectAction::Modify,
                target,
                RiskLevel::Medium,
                RecoveryProspect::Limited,
            )
        }),
        _ => None,
    })
}

// Keeping this declarative table together makes overlapping shell commands
// auditable and prevents a fallback from silently shadowing a known profile.
#[allow(clippy::too_many_lines)]
fn profile_file_or_process_command(
    executable: &str,
    args: &[String],
    segment: &str,
) -> Option<CommandProfile> {
    let profile = |action, target: String, level, recovery| CommandProfile {
        effect: RiskEffect { action, target },
        level,
        recovery,
        privilege: PrivilegeRequirement::CurrentUser,
    };
    let operands = || non_option_operands(args);
    match executable {
        "pwd" | "which" | "where" | "type" | "whence" | "man" | "help" | "stat" | "wc" | "head"
        | "tail" | "less" | "more" | "grep" | "egrep" | "fgrep" | "rg" | "jq" | "yq" | "ps"
        | "top" | "du" | "df" | "mdfind" => Some(profile(
            EffectAction::Read,
            "local files or process metadata".to_owned(),
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "ls" | "tree" => Some(profile(
            EffectAction::Read,
            target_or(&operands(), "directory contents"),
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "cat" => Some(profile(
            EffectAction::Read,
            target_or(&operands(), "standard input"),
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "echo" | "printf" if !has_file_redirection(segment) => Some(profile(
            EffectAction::NoneDetected,
            "terminal output only".to_owned(),
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "mkdir" => Some(profile(
            EffectAction::Create,
            target_or(&operands(), "supplied directories"),
            RiskLevel::Low,
            RecoveryProspect::Reversible,
        )),
        "touch" => Some(profile(
            EffectAction::Modify,
            target_or(&operands(), "supplied paths"),
            RiskLevel::Low,
            RecoveryProspect::Reversible,
        )),
        "cp" => Some(profile(
            EffectAction::Create,
            last_operand(args).unwrap_or("destination path").to_owned(),
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        "mv" => Some(profile(
            EffectAction::Modify,
            last_operand(args).unwrap_or("destination path").to_owned(),
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        "rm" | "rmdir" | "unlink" => Some(profile(
            EffectAction::Delete,
            target_or(&operands(), "supplied paths"),
            RiskLevel::Medium,
            RecoveryProspect::Irreversible,
        )),
        "truncate" => Some(profile(
            EffectAction::Modify,
            last_operand(args).unwrap_or("supplied file").to_owned(),
            RiskLevel::High,
            RecoveryProspect::Irreversible,
        )),
        "chmod" | "chown" | "chgrp" => Some(profile(
            EffectAction::Modify,
            permission_targets(args),
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        "kill" | "pkill" | "killall" => Some(profile(
            EffectAction::Process,
            target_or(&operands(), "target processes"),
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        "curl" | "wget" => {
            let output = download_output(executable, args);
            Some(profile(
                if output.is_some() {
                    EffectAction::Create
                } else {
                    EffectAction::Network
                },
                output.unwrap_or("remote resource").to_owned(),
                RiskLevel::Low,
                if output.is_some() {
                    RecoveryProspect::Reversible
                } else {
                    RecoveryProspect::NotApplicable
                },
            ))
        }
        "open" => Some(profile(
            EffectAction::Execute,
            target_or(&operands(), "supplied application or document"),
            RiskLevel::Medium,
            RecoveryProspect::Unknown,
        )),
        _ => None,
    }
}

fn profile_git(args: &[String]) -> Option<CommandProfile> {
    let subcommand = first_operand(args)?;
    let profile = |action, target: &str, level, recovery| CommandProfile {
        effect: RiskEffect {
            action,
            target: target.to_owned(),
        },
        level,
        recovery,
        privilege: PrivilegeRequirement::CurrentUser,
    };
    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "ls-files" | "rev-parse" => Some(profile(
            EffectAction::Read,
            "local Git metadata and files",
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "add" => Some(profile(
            EffectAction::Modify,
            "Git index",
            RiskLevel::Low,
            RecoveryProspect::Reversible,
        )),
        "commit" => Some(profile(
            EffectAction::Create,
            "local Git history",
            RiskLevel::Low,
            RecoveryProspect::Reversible,
        )),
        "reset" | "checkout" | "switch" | "restore" => Some(profile(
            EffectAction::Modify,
            "Git index and current worktree",
            if args.iter().any(|arg| arg == "--hard") {
                RiskLevel::High
            } else {
                RiskLevel::Medium
            },
            RecoveryProspect::Limited,
        )),
        "clean" if has_git_dry_run(args) => Some(profile(
            EffectAction::Read,
            "untracked files in the current worktree",
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "clean" => Some(profile(
            EffectAction::Delete,
            "untracked files in the current worktree",
            RiskLevel::High,
            RecoveryProspect::Irreversible,
        )),
        "push" => Some(profile(
            EffectAction::Network,
            "destination remote branch history",
            if args.iter().any(|arg| arg == "--force" || arg == "-f") {
                RiskLevel::High
            } else {
                RiskLevel::Low
            },
            RecoveryProspect::Limited,
        )),
        "pull" | "fetch" | "merge" | "rebase" => Some(profile(
            EffectAction::Modify,
            "local Git refs and possibly the worktree",
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        _ => None,
    }
}

fn profile_defaults(args: &[String]) -> Option<CommandProfile> {
    let action = first_operand(args)?;
    let target = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .nth(1)
        .map_or("macOS preference domain", String::as_str)
        .to_owned();
    match action {
        "read" | "find" | "domains" => Some(simple_profile(
            EffectAction::Read,
            target,
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "write" | "delete" | "rename" => Some(simple_profile(
            EffectAction::Modify,
            target,
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        _ => None,
    }
}

fn profile_diskutil(args: &[String]) -> Option<CommandProfile> {
    let action = first_operand(args)?.to_ascii_lowercase();
    let apfs_action = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .nth(1)
        .map(|value| value.to_ascii_lowercase());
    match (action.as_str(), apfs_action.as_deref()) {
        ("list" | "info" | "activity", _) | ("apfs", Some("list" | "listcryptousers")) => {
            Some(simple_profile(
                EffectAction::Read,
                "disk and volume metadata".to_owned(),
                RiskLevel::Low,
                RecoveryProspect::NotApplicable,
            ))
        }
        ("erasedisk" | "erasevolume" | "partitiondisk" | "zerodisk" | "randomdisk", _)
        | ("apfs", Some("deletevolume" | "deletecontainer" | "erasevolume")) => {
            Some(CommandProfile {
                effect: RiskEffect {
                    action: EffectAction::System,
                    target: last_operand(args).unwrap_or("disk or volume").to_owned(),
                },
                level: RiskLevel::Critical,
                recovery: RecoveryProspect::Irreversible,
                privilege: PrivilegeRequirement::ElevatedLikely,
            })
        }
        ("mount" | "unmount" | "eject" | "rename" | "repairdisk" | "repairvolume", _)
        | (
            "apfs",
            Some("addvolume" | "createcontainer" | "resizecontainer" | "changevolumegroup"),
        ) => Some(CommandProfile {
            effect: RiskEffect {
                action: EffectAction::System,
                target: last_operand(args).unwrap_or("disk or volume").to_owned(),
            },
            level: RiskLevel::Medium,
            recovery: RecoveryProspect::Limited,
            privilege: PrivilegeRequirement::ElevatedLikely,
        }),
        ("verifydisk" | "verifyvolume", _) => Some(simple_profile(
            EffectAction::Read,
            "disk and volume metadata".to_owned(),
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        _ => None,
    }
}

fn profile_package_manager(target: &str, args: &[String]) -> Option<CommandProfile> {
    let subcommand = first_operand(args)?;
    match subcommand {
        "list" | "ls" | "outdated" | "info" | "show" => Some(simple_profile(
            EffectAction::Read,
            target.to_owned(),
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "install" | "add" | "remove" | "uninstall" | "upgrade" | "update" | "sync" => {
            Some(simple_profile(
                EffectAction::Modify,
                target.to_owned(),
                RiskLevel::Medium,
                RecoveryProspect::Limited,
            ))
        }
        _ => None,
    }
}

fn profile_docker(args: &[String]) -> Option<CommandProfile> {
    let subcommand = first_operand(args)?;
    let operation = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .nth(1)
        .map(String::as_str);
    match subcommand {
        "ps" | "images" | "logs" | "inspect" | "stats" => Some(simple_profile(
            EffectAction::Read,
            "local Docker state",
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "rm" | "rmi" | "prune" => Some(simple_profile(
            EffectAction::Delete,
            "Docker containers, images, or unused data",
            RiskLevel::High,
            RecoveryProspect::Irreversible,
        )),
        "container" | "image" | "volume" | "network" | "system"
            if matches!(operation, Some("rm" | "prune")) =>
        {
            Some(simple_profile(
                EffectAction::Delete,
                "Docker containers, images, volumes, networks, or unused data",
                RiskLevel::High,
                RecoveryProspect::Irreversible,
            ))
        }
        "compose" if operation == Some("down") => Some(simple_profile(
            EffectAction::Delete,
            "Compose containers and networks (and volumes when requested)",
            RiskLevel::High,
            RecoveryProspect::Limited,
        )),
        "run" | "build" | "compose" => Some(simple_profile(
            EffectAction::Execute,
            "Docker containers, images, volumes, or networks",
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        _ => None,
    }
}

fn profile_kubectl(args: &[String]) -> Option<CommandProfile> {
    let subcommand = first_operand(args)?;
    match subcommand {
        "get" | "describe" | "logs" | "explain" | "api-resources" => Some(simple_profile(
            EffectAction::Read,
            "current Kubernetes context",
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "delete" if has_long_dry_run(args) => Some(simple_profile(
            EffectAction::Read,
            "resources in the current Kubernetes context",
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "delete" => Some(simple_profile(
            EffectAction::Delete,
            "resources in the current Kubernetes context",
            RiskLevel::High,
            RecoveryProspect::Limited,
        )),
        "apply" | "create" | "replace" | "patch" | "scale" | "rollout" => Some(simple_profile(
            EffectAction::Modify,
            "resources in the current Kubernetes context",
            RiskLevel::Medium,
            RecoveryProspect::Limited,
        )),
        _ => None,
    }
}

fn profile_launchctl(args: &[String]) -> Option<CommandProfile> {
    let subcommand = first_operand(args)?;
    match subcommand {
        "list" | "print" | "print-cache" | "procinfo" => Some(simple_profile(
            EffectAction::Read,
            "launchd service state",
            RiskLevel::Low,
            RecoveryProspect::NotApplicable,
        )),
        "bootstrap" | "bootout" | "enable" | "disable" | "kickstart" | "load" | "unload" => {
            Some(simple_profile(
                EffectAction::System,
                "launchd services",
                RiskLevel::Medium,
                RecoveryProspect::Reversible,
            ))
        }
        _ => None,
    }
}

fn simple_profile(
    action: EffectAction,
    target: impl Into<String>,
    level: RiskLevel,
    recovery: RecoveryProspect,
) -> CommandProfile {
    CommandProfile {
        effect: RiskEffect {
            action,
            target: target.into(),
        },
        level,
        recovery,
        privilege: PrivilegeRequirement::CurrentUser,
    }
}

fn effect_from_finding(finding: &SafetyFinding) -> Option<RiskEffect> {
    let (action, fallback) = match finding.rule_id.as_str() {
        rule if rule.starts_with("rm.") => (EffectAction::Delete, "supplied filesystem paths"),
        "disk.mkfs"
        | "disk.dd-device"
        | "disk.diskutil-erase"
        | "disk.diskutil-volume"
        | "disk.raw-redirect" => (EffectAction::System, "disk device or volume data"),
        "file.dd-overwrite" | "file.truncate" => (EffectAction::Modify, "output file data"),
        "git.reset-hard" => (EffectAction::Modify, "Git index and tracked worktree files"),
        "git.clean-force" => (EffectAction::Delete, "untracked worktree files"),
        "git.push-force" | "git.push-force-with-lease" => {
            (EffectAction::Network, "destination remote branch history")
        }
        rule if rule.starts_with("permissions.") => (
            EffectAction::Modify,
            "permissions or ownership of supplied paths",
        ),
        "process.sigkill" => (EffectAction::Process, "target process state"),
        "find.delete" => (EffectAction::Delete, "paths matched by find"),
        "sql.drop-database" => (EffectAction::Delete, "database and all contained objects"),
        "sql.drop-table" => (EffectAction::Delete, "table definition and stored rows"),
        "shell.download-pipe" => (
            EffectAction::Execute,
            "this Mac using downloaded shell code",
        ),
        "shell.fork-bomb" => (EffectAction::System, "CPU, memory, and process capacity"),
        "system.power" => (EffectAction::System, "running sessions and unsaved work"),
        _ => return None,
    };
    Some(RiskEffect {
        action,
        target: fallback.to_owned(),
    })
}

fn recovery_from_rule(rule: &str) -> RecoveryProspect {
    match rule {
        rule if rule.starts_with("rm.") => RecoveryProspect::Irreversible,
        rule if rule.starts_with("disk.") => RecoveryProspect::Irreversible,
        "file.dd-overwrite" | "file.truncate" | "git.clean-force" | "find.delete"
        | "sql.drop-database" | "sql.drop-table" => RecoveryProspect::Irreversible,
        "git.reset-hard"
        | "git.push-force"
        | "git.push-force-with-lease"
        | "process.sigkill"
        | "shell.download-pipe"
        | "system.power" => RecoveryProspect::Limited,
        rule if rule.starts_with("permissions.") => RecoveryProspect::Reversible,
        _ => RecoveryProspect::Unknown,
    }
}

fn first_operand(args: &[String]) -> Option<&str> {
    args.iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str)
}

fn last_operand(args: &[String]) -> Option<&str> {
    args.iter()
        .rev()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str)
}

fn non_option_operands(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

fn permission_targets(args: &[String]) -> String {
    let operands = non_option_operands(args);
    target_or(
        &operands.into_iter().skip(1).collect::<Vec<_>>(),
        "supplied paths",
    )
}

fn target_or(targets: &[String], fallback: &str) -> String {
    if targets.is_empty() {
        fallback.to_owned()
    } else {
        targets.join(", ")
    }
}

fn download_output<'a>(executable: &str, args: &'a [String]) -> Option<&'a str> {
    match executable {
        "curl" => {
            if args
                .iter()
                .any(|arg| matches!(arg.as_str(), "-O" | "--remote-name"))
            {
                return Some("remote-named download");
            }
            args.windows(2)
                .find(|pair| matches!(pair[0].as_str(), "-o" | "--output"))
                .map(|pair| pair[1].as_str())
        }
        "wget" => args
            .windows(2)
            .find(|pair| matches!(pair[0].as_str(), "-O" | "--output-document"))
            .map(|pair| pair[1].as_str())
            .or(Some("downloaded file")),
        _ => None,
    }
}

fn has_long_dry_run(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--dry-run"
            || arg
                .strip_prefix("--dry-run=")
                .is_some_and(|value| value != "none")
    })
}

fn has_git_dry_run(args: &[String]) -> bool {
    has_long_dry_run(args)
        || args.iter().any(|arg| {
            arg == "-n"
                || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('n'))
        })
}

fn has_file_redirection(segment: &str) -> bool {
    redirected_target(segment).is_some()
}

fn redirected_target(segment: &str) -> Option<String> {
    let tokens = shell_words::split(segment).unwrap_or_else(|_| lenient_tokens(segment));
    let separated = tokens.windows(2).find_map(|pair| {
        matches!(pair[0].as_str(), ">" | ">>" | "1>" | "2>").then(|| pair[1].clone())
    });
    separated.or_else(|| {
        tokens.iter().find_map(|token| {
            if token.starts_with("<<") || token.starts_with(">(") {
                return None;
            }
            [
                "1>>", "2>>", "&>>", ">>", "1>|", "2>|", "&>|", ">|", "1>", "2>", "&>", ">",
            ]
            .iter()
            .find_map(|prefix| token.strip_prefix(prefix))
            .filter(|target| !target.is_empty() && !target.starts_with('&'))
            .map(str::to_owned)
        })
    })
}

fn push_effect(effects: &mut Vec<RiskEffect>, effect: RiskEffect) {
    if !effects.contains(&effect) {
        effects.push(effect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SafetyConfig, SafetyMode};

    #[test]
    fn destructive_git_command_has_concrete_impact_and_recovery() {
        let report = SafetyEngine::new().risk_lens("git reset --hard");
        assert_eq!(report.level, Some(RiskLevel::High));
        assert_eq!(report.coverage, AnalysisCoverage::Recognized);
        assert_eq!(report.privilege, PrivilegeRequirement::CurrentUser);
        assert_eq!(report.recovery, RecoveryProspect::Limited);
        assert!(report.effects.iter().any(|effect| {
            effect.action == EffectAction::Modify && effect.target.contains("worktree")
        }));
        assert_eq!(report.rule_ids, ["git.reset-hard"]);
    }

    #[test]
    fn exact_rm_target_and_sudo_are_visible() {
        let report = SafetyEngine::new().risk_lens("sudo rm -rf ~/Downloads/cache");
        assert_eq!(report.level, Some(RiskLevel::High));
        assert_eq!(report.privilege, PrivilegeRequirement::Administrator);
        assert_eq!(report.recovery, RecoveryProspect::Irreversible);
        assert!(report.effects.iter().any(|effect| {
            effect.action == EffectAction::Delete && effect.target.contains("~/Downloads/cache")
        }));
    }

    #[test]
    fn unknown_commands_are_unrated_instead_of_low_risk() {
        let report = SafetyEngine::new().risk_lens("company-deploy production");
        assert_eq!(report.level, None);
        assert_eq!(report.coverage, AnalysisCoverage::Unknown);
        assert_eq!(report.privilege, PrivilegeRequirement::Unknown);
        assert_eq!(report.recovery, RecoveryProspect::Unknown);
        assert_eq!(report.effects[0].action, EffectAction::Unknown);

        let compound = SafetyEngine::new().risk_lens("git status; company-deploy production");
        assert_eq!(compound.level, None);
        assert_eq!(compound.coverage, AnalysisCoverage::Partial);
    }

    #[test]
    fn read_only_and_cloud_mutations_are_distinguished() {
        let read = SafetyEngine::new().risk_lens("git status");
        assert_eq!(read.level, Some(RiskLevel::Low));
        assert_eq!(read.recovery, RecoveryProspect::NotApplicable);
        assert_eq!(read.effects[0].action, EffectAction::Read);

        let cloud = SafetyEngine::new().risk_lens("kubectl delete pod api-0");
        assert_eq!(cloud.level, Some(RiskLevel::High));
        assert_eq!(cloud.effects[0].action, EffectAction::Delete);
        assert!(cloud.effects[0].target.contains("Kubernetes"));
    }

    #[test]
    fn a_safety_finding_keeps_an_unprofiled_command_rated() {
        let report = SafetyEngine::new().risk_lens("dd if=/tmp/source of=/tmp/destination");
        assert_eq!(report.level, Some(RiskLevel::High));
        assert_eq!(report.coverage, AnalysisCoverage::Partial);
        assert!(report.rule_ids.contains(&"file.dd-overwrite".to_owned()));
        assert!(
            report
                .effects
                .iter()
                .any(|effect| effect.action == EffectAction::Modify)
        );
    }

    #[test]
    fn disk_and_nested_docker_operations_are_not_misclassified_as_reads() {
        let disk = SafetyEngine::new().risk_lens("diskutil apfs addVolume disk3 APFS Work");
        assert_eq!(disk.level, Some(RiskLevel::Medium));
        assert_eq!(disk.effects[0].action, EffectAction::System);
        assert_eq!(disk.recovery, RecoveryProspect::Limited);

        let docker = SafetyEngine::new().risk_lens("docker system prune -af");
        assert_eq!(docker.level, Some(RiskLevel::High));
        assert_eq!(docker.effects[0].action, EffectAction::Delete);
        assert_eq!(docker.recovery, RecoveryProspect::Irreversible);
    }

    #[test]
    fn dry_runs_are_reported_as_reads_not_mutations() {
        for command in [
            "git clean -fdn",
            "kubectl delete pod api-0 --dry-run=client",
        ] {
            let report = SafetyEngine::new().risk_lens(command);
            assert_eq!(report.level, Some(RiskLevel::Low));
            assert_eq!(report.effects[0].action, EffectAction::Read);
            assert_eq!(report.recovery, RecoveryProspect::NotApplicable);
        }
    }

    #[test]
    fn shell_redirection_is_an_independent_write_effect() {
        for command in ["git status > report.txt", "git status 2>errors.txt"] {
            let report = SafetyEngine::new().risk_lens(command);
            assert_eq!(report.level, Some(RiskLevel::Medium));
            assert_eq!(report.coverage, AnalysisCoverage::Recognized);
            assert!(
                report
                    .effects
                    .iter()
                    .any(|effect| effect.action == EffectAction::Read)
            );
            assert!(report.effects.iter().any(|effect| {
                effect.action == EffectAction::Modify
                    && matches!(effect.target.as_str(), "report.txt" | "errors.txt")
            }));
            assert_eq!(report.recovery, RecoveryProspect::Limited);
        }
    }

    #[test]
    fn disabled_automatic_rules_remain_explicit() {
        let engine = SafetyEngine::with_config(SafetyConfig {
            enabled: false,
            mode: SafetyMode::Warn,
        });
        let report = engine.risk_lens("rm notes.txt");
        assert!(!report.safety_rules_enabled);
        assert_eq!(report.level, Some(RiskLevel::Medium));
        assert_eq!(report.recovery, RecoveryProspect::Irreversible);
    }

    #[test]
    fn oversized_input_is_unrated_instead_of_partially_scanned() {
        let command = "x".repeat(MAX_RISK_LENS_CHARS + 1);
        let report = SafetyEngine::new().risk_lens(&command);
        assert_eq!(report.level, None);
        assert_eq!(report.coverage, AnalysisCoverage::Unknown);
        assert!(report.effects[0].target.contains("analysis limit"));
    }
}
