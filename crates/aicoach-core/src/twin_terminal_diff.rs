//! Pure comparison primitives for two explicitly captured terminal environments.
//!
//! Collection belongs to the CLI/daemon layer. This module never reads the
//! process environment, resolves `PATH`, or executes a tool to discover its
//! version.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwinTerminalSnapshot {
    #[serde(default)]
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rosetta_translated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homebrew_prefix: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xcode_developer_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_environment: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conda_environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<TwinTerminalGitSnapshot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commands: BTreeMap<String, TwinTerminalCommandSnapshot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sensitive_environment: BTreeMap<String, SensitiveEnvironmentValue>,
}

impl TwinTerminalSnapshot {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwinTerminalGitSnapshot {
    #[serde(default)]
    pub repository: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub detached: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwinTerminalCommandSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<PathBuf>,
}

/// A non-secret observation of a sensitive environment variable.
///
/// `digest` hashes the provided value immediately. The original value is not
/// retained, serialized, included in diffs, or shown by `Debug`.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SensitiveEnvironmentValue(SensitiveEnvironmentObservation);

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "digest", rename_all = "camelCase")]
enum SensitiveEnvironmentObservation {
    Present,
    Sha256([u8; 32]),
}

impl SensitiveEnvironmentValue {
    pub const fn present() -> Self {
        Self(SensitiveEnvironmentObservation::Present)
    }

    pub fn digest(variable_name: &str, value: impl AsRef<[u8]>) -> Self {
        let variable_name = variable_name.as_bytes();
        let value = value.as_ref();
        let mut hasher = Sha256::new();
        hasher.update(b"aicoach-twin-terminal-sensitive-env-v1\0");
        hasher.update(variable_name.len().to_be_bytes());
        hasher.update(variable_name);
        hasher.update(value.len().to_be_bytes());
        hasher.update(value);
        Self(SensitiveEnvironmentObservation::Sha256(
            hasher.finalize().into(),
        ))
    }
}

impl fmt::Debug for SensitiveEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            SensitiveEnvironmentObservation::Present => "SensitiveEnvironmentValue(Present)",
            SensitiveEnvironmentObservation::Sha256(_) => {
                "SensitiveEnvironmentValue(Sha256([NON_SECRET_DIGEST]))"
            }
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum TwinTerminalDiffKind {
    Changed,
    OnlyBaseline,
    OnlyCurrent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "type", content = "name", rename_all = "camelCase")]
pub enum TwinTerminalDiffField {
    WorkingDirectory,
    TerminalProgram,
    ShellArchitecture,
    RosettaTranslation,
    HomebrewPrefix,
    XcodeDeveloperDirectory,
    VirtualEnvironment,
    CondaEnvironment,
    GitRepository,
    GitBranch,
    GitDetached,
    Command(String),
    SensitiveEnvironment(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum TwinTerminalDiffValue {
    Path(PathBuf),
    Text(String),
    Boolean(bool),
    Command(TwinTerminalCommandSnapshot),
    Sensitive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwinTerminalDiffEntry {
    pub field: TwinTerminalDiffField,
    pub kind: TwinTerminalDiffKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<TwinTerminalDiffValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<TwinTerminalDiffValue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwinTerminalDiffReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<TwinTerminalDiffEntry>,
}

impl TwinTerminalDiffReport {
    pub fn between(baseline: &TwinTerminalSnapshot, current: &TwinTerminalSnapshot) -> Self {
        compare_twin_terminal_snapshots(baseline, current)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn compare_twin_terminal_snapshots(
    baseline: &TwinTerminalSnapshot,
    current: &TwinTerminalSnapshot,
) -> TwinTerminalDiffReport {
    let mut entries = Vec::new();
    compare_value(
        &mut entries,
        TwinTerminalDiffField::WorkingDirectory,
        Some(TwinTerminalDiffValue::Path(baseline.cwd.clone())),
        Some(TwinTerminalDiffValue::Path(current.cwd.clone())),
    );
    compare_text(
        &mut entries,
        TwinTerminalDiffField::TerminalProgram,
        baseline.terminal_program.as_ref(),
        current.terminal_program.as_ref(),
    );
    compare_text(
        &mut entries,
        TwinTerminalDiffField::ShellArchitecture,
        baseline.shell_architecture.as_ref(),
        current.shell_architecture.as_ref(),
    );
    compare_value(
        &mut entries,
        TwinTerminalDiffField::RosettaTranslation,
        baseline
            .rosetta_translated
            .map(TwinTerminalDiffValue::Boolean),
        current
            .rosetta_translated
            .map(TwinTerminalDiffValue::Boolean),
    );
    compare_path(
        &mut entries,
        TwinTerminalDiffField::HomebrewPrefix,
        baseline.homebrew_prefix.as_ref(),
        current.homebrew_prefix.as_ref(),
    );
    compare_path(
        &mut entries,
        TwinTerminalDiffField::XcodeDeveloperDirectory,
        baseline.xcode_developer_dir.as_ref(),
        current.xcode_developer_dir.as_ref(),
    );
    compare_path(
        &mut entries,
        TwinTerminalDiffField::VirtualEnvironment,
        baseline.virtual_environment.as_ref(),
        current.virtual_environment.as_ref(),
    );
    compare_text(
        &mut entries,
        TwinTerminalDiffField::CondaEnvironment,
        baseline.conda_environment.as_ref(),
        current.conda_environment.as_ref(),
    );
    compare_git(&mut entries, baseline.git.as_ref(), current.git.as_ref());
    compare_commands(&mut entries, &baseline.commands, &current.commands);
    compare_sensitive_environment(
        &mut entries,
        &baseline.sensitive_environment,
        &current.sensitive_environment,
    );
    entries.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then(left.kind.cmp(&right.kind))
    });
    TwinTerminalDiffReport { entries }
}

fn compare_git(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    baseline: Option<&TwinTerminalGitSnapshot>,
    current: Option<&TwinTerminalGitSnapshot>,
) {
    compare_value(
        entries,
        TwinTerminalDiffField::GitRepository,
        baseline.map(|git| TwinTerminalDiffValue::Path(git.repository.clone())),
        current.map(|git| TwinTerminalDiffValue::Path(git.repository.clone())),
    );
    let (Some(baseline), Some(current)) = (baseline, current) else {
        return;
    };
    compare_text(
        entries,
        TwinTerminalDiffField::GitBranch,
        baseline.branch.as_ref(),
        current.branch.as_ref(),
    );
    compare_value(
        entries,
        TwinTerminalDiffField::GitDetached,
        Some(TwinTerminalDiffValue::Boolean(baseline.detached)),
        Some(TwinTerminalDiffValue::Boolean(current.detached)),
    );
}

fn compare_commands(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    baseline: &BTreeMap<String, TwinTerminalCommandSnapshot>,
    current: &BTreeMap<String, TwinTerminalCommandSnapshot>,
) {
    let commands = baseline
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for command in commands {
        compare_value(
            entries,
            TwinTerminalDiffField::Command(command.clone()),
            baseline
                .get(&command)
                .cloned()
                .map(TwinTerminalDiffValue::Command),
            current
                .get(&command)
                .cloned()
                .map(TwinTerminalDiffValue::Command),
        );
    }
}

fn compare_sensitive_environment(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    baseline: &BTreeMap<String, SensitiveEnvironmentValue>,
    current: &BTreeMap<String, SensitiveEnvironmentValue>,
) {
    let names = baseline
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for name in names {
        let before = baseline.get(&name);
        let after = current.get(&name);
        if before == after {
            continue;
        }
        push_entry(
            entries,
            TwinTerminalDiffField::SensitiveEnvironment(name),
            before.map(|_| TwinTerminalDiffValue::Sensitive),
            after.map(|_| TwinTerminalDiffValue::Sensitive),
        );
    }
}

fn compare_text(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    field: TwinTerminalDiffField,
    baseline: Option<&String>,
    current: Option<&String>,
) {
    compare_value(
        entries,
        field,
        baseline.cloned().map(TwinTerminalDiffValue::Text),
        current.cloned().map(TwinTerminalDiffValue::Text),
    );
}

fn compare_path(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    field: TwinTerminalDiffField,
    baseline: Option<&PathBuf>,
    current: Option<&PathBuf>,
) {
    compare_value(
        entries,
        field,
        baseline.cloned().map(TwinTerminalDiffValue::Path),
        current.cloned().map(TwinTerminalDiffValue::Path),
    );
}

fn compare_value(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    field: TwinTerminalDiffField,
    baseline: Option<TwinTerminalDiffValue>,
    current: Option<TwinTerminalDiffValue>,
) {
    if baseline == current {
        return;
    }
    push_entry(entries, field, baseline, current);
}

fn push_entry(
    entries: &mut Vec<TwinTerminalDiffEntry>,
    field: TwinTerminalDiffField,
    baseline: Option<TwinTerminalDiffValue>,
    current: Option<TwinTerminalDiffValue>,
) {
    let kind = match (&baseline, &current) {
        (Some(_), Some(_)) => TwinTerminalDiffKind::Changed,
        (Some(_), None) => TwinTerminalDiffKind::OnlyBaseline,
        (None, Some(_)) => TwinTerminalDiffKind::OnlyCurrent,
        (None, None) => return,
    };
    entries.push(TwinTerminalDiffEntry {
        field,
        kind,
        baseline,
        current,
    });
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::*;

    fn command(path: &str) -> TwinTerminalCommandSnapshot {
        TwinTerminalCommandSnapshot {
            resolved_path: Some(PathBuf::from(path)),
        }
    }

    fn git(root: &str, branch: &str) -> TwinTerminalGitSnapshot {
        TwinTerminalGitSnapshot {
            repository: PathBuf::from(root),
            branch: Some(branch.to_owned()),
            ..TwinTerminalGitSnapshot::default()
        }
    }

    #[test]
    fn compares_explicit_terminal_observations_in_stable_field_order() {
        let mut baseline = TwinTerminalSnapshot::new("/work/intel");
        baseline.terminal_program = Some("Apple_Terminal".to_owned());
        baseline.shell_architecture = Some("x86_64".to_owned());
        baseline.rosetta_translated = Some(true);
        baseline.homebrew_prefix = Some(PathBuf::from("/usr/local"));
        baseline.xcode_developer_dir = Some(PathBuf::from("/Applications/Xcode.app/old"));
        baseline.conda_environment = Some("analytics".to_owned());
        baseline.git = Some(git("/work/repo", "main"));
        baseline.commands = BTreeMap::from([
            ("python".to_owned(), command("/usr/local/bin/python")),
            ("git".to_owned(), command("/usr/bin/git")),
        ]);

        let mut current = TwinTerminalSnapshot::new("/work/arm");
        current.terminal_program = Some("iTerm.app".to_owned());
        current.shell_architecture = Some("arm64".to_owned());
        current.rosetta_translated = Some(false);
        current.homebrew_prefix = Some(PathBuf::from("/opt/homebrew"));
        current.virtual_environment = Some(PathBuf::from("/work/arm/.venv"));
        current.conda_environment = Some("ml".to_owned());
        current.git = Some(git("/work/other", "feature/twin"));
        current.commands = BTreeMap::from([
            ("cargo".to_owned(), command("/Users/me/.cargo/bin/cargo")),
            ("git".to_owned(), command("/opt/homebrew/bin/git")),
        ]);

        let report = compare_twin_terminal_snapshots(&baseline, &current);
        let fields = report
            .entries
            .iter()
            .map(|entry| entry.field.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            fields,
            vec![
                TwinTerminalDiffField::WorkingDirectory,
                TwinTerminalDiffField::TerminalProgram,
                TwinTerminalDiffField::ShellArchitecture,
                TwinTerminalDiffField::RosettaTranslation,
                TwinTerminalDiffField::HomebrewPrefix,
                TwinTerminalDiffField::XcodeDeveloperDirectory,
                TwinTerminalDiffField::VirtualEnvironment,
                TwinTerminalDiffField::CondaEnvironment,
                TwinTerminalDiffField::GitRepository,
                TwinTerminalDiffField::GitBranch,
                TwinTerminalDiffField::Command("cargo".to_owned()),
                TwinTerminalDiffField::Command("git".to_owned()),
                TwinTerminalDiffField::Command("python".to_owned()),
            ]
        );
        assert_eq!(report.entries[5].kind, TwinTerminalDiffKind::OnlyBaseline);
        assert_eq!(report.entries[6].kind, TwinTerminalDiffKind::OnlyCurrent);
        assert_eq!(report.entries[10].kind, TwinTerminalDiffKind::OnlyCurrent);
        assert_eq!(report.entries[11].kind, TwinTerminalDiffKind::Changed);
        assert_eq!(report.entries[12].kind, TwinTerminalDiffKind::OnlyBaseline);
    }

    #[test]
    fn sensitive_environment_is_presence_or_digest_and_never_enters_the_report() {
        let secret = "not-for-storage-or-display";
        let unchanged = SensitiveEnvironmentValue::digest("UNCHANGED_TOKEN", secret);
        let mut baseline = TwinTerminalSnapshot::new("/work");
        baseline.sensitive_environment = BTreeMap::from([
            (
                "ADDED_TOKEN".to_owned(),
                SensitiveEnvironmentValue::present(),
            ),
            (
                "CHANGED_TOKEN".to_owned(),
                SensitiveEnvironmentValue::digest("CHANGED_TOKEN", secret),
            ),
            ("UNCHANGED_TOKEN".to_owned(), unchanged.clone()),
        ]);
        let mut current = TwinTerminalSnapshot::new("/work");
        current.sensitive_environment = BTreeMap::from([
            (
                "CHANGED_TOKEN".to_owned(),
                SensitiveEnvironmentValue::digest("CHANGED_TOKEN", "different-secret"),
            ),
            (
                "CURRENT_TOKEN".to_owned(),
                SensitiveEnvironmentValue::present(),
            ),
            ("UNCHANGED_TOKEN".to_owned(), unchanged),
        ]);

        let report = compare_twin_terminal_snapshots(&baseline, &current);
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| (&entry.field, entry.kind))
                .collect::<Vec<_>>(),
            vec![
                (
                    &TwinTerminalDiffField::SensitiveEnvironment("ADDED_TOKEN".to_owned()),
                    TwinTerminalDiffKind::OnlyBaseline,
                ),
                (
                    &TwinTerminalDiffField::SensitiveEnvironment("CHANGED_TOKEN".to_owned()),
                    TwinTerminalDiffKind::Changed,
                ),
                (
                    &TwinTerminalDiffField::SensitiveEnvironment("CURRENT_TOKEN".to_owned()),
                    TwinTerminalDiffKind::OnlyCurrent,
                ),
            ]
        );

        let snapshot_json = serde_json::to_string(&baseline).unwrap();
        let report_json = serde_json::to_string(&report).unwrap();
        let debug = format!("{baseline:?} {report:?}");
        assert!(!snapshot_json.contains(secret));
        assert!(!report_json.contains(secret));
        assert!(!debug.contains(secret));
        assert!(!report_json.contains("digest"));
    }

    #[test]
    fn serde_defaults_keep_minimal_snapshots_and_reports_compatible() {
        let snapshot: TwinTerminalSnapshot = serde_json::from_str(r#"{"cwd":"/tmp"}"#).unwrap();
        assert_eq!(snapshot.cwd, PathBuf::from("/tmp"));
        assert!(snapshot.commands.is_empty());
        assert!(snapshot.sensitive_environment.is_empty());

        let report: TwinTerminalDiffReport = serde_json::from_str("{}").unwrap();
        assert!(report.is_empty());
        assert_eq!(
            serde_json::from_str::<TwinTerminalSnapshot>(
                &serde_json::to_string(&snapshot).unwrap()
            )
            .unwrap(),
            snapshot
        );
    }

    #[test]
    fn equal_snapshots_have_no_diff() {
        let snapshot = TwinTerminalSnapshot::new("/work");
        assert!(TwinTerminalDiffReport::between(&snapshot, &snapshot).is_empty());
    }
}
