//! Pure, provider-free comparison of safe shell and Git metadata.

use std::{collections::BTreeMap, path::PathBuf};

use crate::GitContext;

/// The small set of changes that can plausibly explain why a command now
/// behaves differently from the most recent successful command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentDriftKind {
    WorkingDirectory,
    PythonEnvironment,
    CondaEnvironment,
    GitRepository,
    GitBranch,
    GitWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentDrift {
    pub kind: EnvironmentDriftKind,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A point-in-time snapshot. Environment input is expected to have passed the
/// IPC allowlist; no arbitrary process environment belongs here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSnapshot {
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    /// Distinguishes a completed "not a repository" probe from a timeout.
    pub git_observed: bool,
    pub git: Option<GitContext>,
}

impl EnvironmentSnapshot {
    pub fn new(cwd: impl Into<PathBuf>, environment: BTreeMap<String, String>) -> Self {
        Self {
            cwd: cwd.into(),
            environment,
            git_observed: false,
            git: None,
        }
    }

    #[must_use]
    pub fn with_git_probe(mut self, observed: bool, git: Option<GitContext>) -> Self {
        self.git_observed = observed;
        self.git = git;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvironmentDriftReport {
    pub changes: Vec<EnvironmentDrift>,
}

impl EnvironmentDriftReport {
    pub fn between(previous: &EnvironmentSnapshot, current: &EnvironmentSnapshot) -> Self {
        let mut changes = Vec::new();
        if previous.cwd != current.cwd {
            changes.push(EnvironmentDrift {
                kind: EnvironmentDriftKind::WorkingDirectory,
                before: Some(previous.cwd.to_string_lossy().into_owned()),
                after: Some(current.cwd.to_string_lossy().into_owned()),
            });
        }
        compare_environment_value(
            &mut changes,
            EnvironmentDriftKind::PythonEnvironment,
            "VIRTUAL_ENV",
            previous,
            current,
        );
        compare_environment_value(
            &mut changes,
            EnvironmentDriftKind::CondaEnvironment,
            "CONDA_DEFAULT_ENV",
            previous,
            current,
        );
        compare_git(&mut changes, previous, current);
        Self { changes }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

fn compare_environment_value(
    changes: &mut Vec<EnvironmentDrift>,
    kind: EnvironmentDriftKind,
    key: &str,
    previous: &EnvironmentSnapshot,
    current: &EnvironmentSnapshot,
) {
    let before = nonempty(previous.environment.get(key));
    let after = nonempty(current.environment.get(key));
    if before != after {
        changes.push(EnvironmentDrift {
            kind,
            before: before.cloned(),
            after: after.cloned(),
        });
    }
}

fn nonempty(value: Option<&String>) -> Option<&String> {
    value.filter(|value| !value.is_empty())
}

fn compare_git(
    changes: &mut Vec<EnvironmentDrift>,
    previous: &EnvironmentSnapshot,
    current: &EnvironmentSnapshot,
) {
    if !previous.git_observed || !current.git_observed {
        return;
    }
    match (&previous.git, &current.git) {
        (None, None) => {}
        (None, Some(current_git)) => changes.push(EnvironmentDrift {
            kind: EnvironmentDriftKind::GitRepository,
            before: None,
            after: Some(current_git.repo_root.to_string_lossy().into_owned()),
        }),
        (Some(previous_git), None) => changes.push(EnvironmentDrift {
            kind: EnvironmentDriftKind::GitRepository,
            before: Some(previous_git.repo_root.to_string_lossy().into_owned()),
            after: None,
        }),
        (Some(previous_git), Some(current_git)) => {
            if previous_git.repo_root != current_git.repo_root {
                changes.push(EnvironmentDrift {
                    kind: EnvironmentDriftKind::GitRepository,
                    before: Some(previous_git.repo_root.to_string_lossy().into_owned()),
                    after: Some(current_git.repo_root.to_string_lossy().into_owned()),
                });
                return;
            }
            let previous_branch = branch_name(previous_git);
            let current_branch = branch_name(current_git);
            if previous_branch != current_branch {
                changes.push(EnvironmentDrift {
                    kind: EnvironmentDriftKind::GitBranch,
                    before: Some(previous_branch),
                    after: Some(current_branch),
                });
            }
            let previous_state = git_state(previous_git);
            let current_state = git_state(current_git);
            if previous_state != current_state {
                changes.push(EnvironmentDrift {
                    kind: EnvironmentDriftKind::GitWorktree,
                    before: Some(previous_state),
                    after: Some(current_state),
                });
            }
        }
    }
}

fn branch_name(git: &GitContext) -> String {
    if git.detached {
        "detached HEAD".to_owned()
    } else {
        git.branch.clone().unwrap_or_else(|| "unknown".to_owned())
    }
}

fn git_state(git: &GitContext) -> String {
    let mut values = Vec::new();
    if git.modified_files > 0 {
        values.push(format!("{} modified", git.modified_files));
    }
    if git.staged_files > 0 {
        values.push(format!("{} staged", git.staged_files));
    }
    if git.untracked_files > 0 {
        values.push(format!("{} untracked", git.untracked_files));
    }
    if git.conflicts > 0 {
        values.push(format!("{} conflicted", git.conflicts));
    }
    if git.ahead > 0 {
        values.push(format!("{} ahead", git.ahead));
    }
    if git.behind > 0 {
        values.push(format!("{} behind", git.behind));
    }
    if values.is_empty() {
        "clean".to_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &str, branch: &str) -> GitContext {
        GitContext {
            repo_root: PathBuf::from(root),
            branch: Some(branch.to_owned()),
            ..GitContext::default()
        }
    }

    #[test]
    fn reports_only_material_allowlisted_shell_changes() {
        let previous = EnvironmentSnapshot::new(
            "/work/old",
            BTreeMap::from([
                ("VIRTUAL_ENV".to_owned(), "/work/old/.venv".to_owned()),
                ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
            ]),
        );
        let current = EnvironmentSnapshot::new(
            "/work/new",
            BTreeMap::from([
                ("CONDA_DEFAULT_ENV".to_owned(), "ml".to_owned()),
                ("LANG".to_owned(), "zh_CN.UTF-8".to_owned()),
            ]),
        );
        let report = EnvironmentDriftReport::between(&previous, &current);
        assert_eq!(report.changes.len(), 3);
        assert_eq!(
            report
                .changes
                .iter()
                .map(|change| change.kind)
                .collect::<Vec<_>>(),
            [
                EnvironmentDriftKind::WorkingDirectory,
                EnvironmentDriftKind::PythonEnvironment,
                EnvironmentDriftKind::CondaEnvironment,
            ]
        );
    }

    #[test]
    fn reports_branch_and_worktree_drift_inside_the_same_repository() {
        let mut previous_git = git("/work/repo", "main");
        previous_git.modified_files = 1;
        let mut current_git = git("/work/repo", "feature");
        current_git.staged_files = 2;
        current_git.ahead = 1;
        let previous = EnvironmentSnapshot::new("/work/repo", BTreeMap::new())
            .with_git_probe(true, Some(previous_git));
        let current = EnvironmentSnapshot::new("/work/repo", BTreeMap::new())
            .with_git_probe(true, Some(current_git));
        let report = EnvironmentDriftReport::between(&previous, &current);
        assert_eq!(report.changes.len(), 2);
        assert_eq!(report.changes[0].kind, EnvironmentDriftKind::GitBranch);
        assert_eq!(report.changes[0].before.as_deref(), Some("main"));
        assert_eq!(report.changes[0].after.as_deref(), Some("feature"));
        assert_eq!(report.changes[1].kind, EnvironmentDriftKind::GitWorktree);
        assert!(
            report.changes[1]
                .after
                .as_deref()
                .is_some_and(|value| value.contains("2 staged") && value.contains("1 ahead"))
        );
    }

    #[test]
    fn incomplete_git_probe_is_omitted_instead_of_inventing_drift() {
        let previous =
            EnvironmentSnapshot::new("/work/repo", BTreeMap::new()).with_git_probe(false, None);
        let current = EnvironmentSnapshot::new("/work/repo", BTreeMap::new())
            .with_git_probe(true, Some(git("/work/repo", "main")));
        assert!(EnvironmentDriftReport::between(&previous, &current).is_empty());
    }

    #[test]
    fn entering_and_leaving_a_repository_are_explicit() {
        let outside = EnvironmentSnapshot::new("/tmp", BTreeMap::new()).with_git_probe(true, None);
        let inside = EnvironmentSnapshot::new("/work/repo", BTreeMap::new())
            .with_git_probe(true, Some(git("/work/repo", "main")));
        let entered = EnvironmentDriftReport::between(&outside, &inside);
        assert!(entered.changes.iter().any(|change| {
            change.kind == EnvironmentDriftKind::GitRepository && change.before.is_none()
        }));
        let left = EnvironmentDriftReport::between(&inside, &outside);
        assert!(left.changes.iter().any(|change| {
            change.kind == EnvironmentDriftKind::GitRepository && change.after.is_none()
        }));
    }
}
