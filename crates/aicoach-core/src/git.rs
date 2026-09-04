use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use thiserror::Error;

use crate::models::GitContext;

/// Collect only repository metadata: no diff, file content, commit message, or
/// full history is read. `Ok(None)` means `cwd` is not inside a Git work tree.
///
/// # Errors
///
/// Returns an error when Git cannot be launched or repository metadata cannot
/// be read. Use [`try_collect_git_context`] for non-critical enrichment.
pub fn collect_git_context(cwd: impl AsRef<Path>) -> Result<Option<GitContext>, GitContextError> {
    let cwd = cwd.as_ref();
    let root_output = run_git(cwd, &["rev-parse", "--show-toplevel"])?;
    if !root_output.status.success() {
        let stderr = String::from_utf8_lossy(&root_output.stderr);
        if stderr.contains("not a git repository") {
            return Ok(None);
        }
        return Err(GitContextError::CommandFailed {
            operation: "discover repository root",
            status: root_output.status.code(),
            stderr: stderr.trim().to_owned(),
        });
    }
    let repo_root = PathBuf::from(String::from_utf8_lossy(&root_output.stdout).trim());

    let status_output = run_git(
        cwd,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ],
    )?;
    if !status_output.status.success() {
        return Err(GitContextError::CommandFailed {
            operation: "read repository status",
            status: status_output.status.code(),
            stderr: String::from_utf8_lossy(&status_output.stderr)
                .trim()
                .to_owned(),
        });
    }
    let mut context =
        parse_porcelain_v2(&String::from_utf8_lossy(&status_output.stdout), repo_root);

    // `remote get-url` reads only local config and avoids sending credentials or
    // making network calls. Credentials embedded in HTTPS URLs are stripped.
    if let Ok(remote_output) = run_git(cwd, &["remote", "get-url", "origin"])
        && remote_output.status.success()
    {
        let remote = String::from_utf8_lossy(&remote_output.stdout);
        let remote = remote.trim();
        if !remote.is_empty() {
            context.remote = Some(sanitize_remote(remote));
        }
    }
    Ok(Some(context))
}

/// Best-effort form for prompt enrichment where Git being absent or broken must
/// never interfere with the terminal workflow.
pub fn try_collect_git_context(cwd: impl AsRef<Path>) -> Option<GitContext> {
    collect_git_context(cwd).ok().flatten()
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, GitContextError> {
    Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(GitContextError::Spawn)
}

fn parse_porcelain_v2(output: &str, repo_root: PathBuf) -> GitContext {
    let mut context = GitContext {
        repo_root,
        ..GitContext::default()
    };
    for line in output.lines() {
        if let Some(branch) = line.strip_prefix("# branch.head ") {
            if branch == "(detached)" {
                context.detached = true;
            } else {
                context.branch = Some(branch.to_owned());
            }
            continue;
        }
        if let Some(counts) = line.strip_prefix("# branch.ab ") {
            for count in counts.split_whitespace() {
                if let Some(ahead) = count.strip_prefix('+') {
                    context.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = count.strip_prefix('-') {
                    context.behind = behind.parse().unwrap_or(0);
                }
            }
            continue;
        }
        if line.starts_with("? ") {
            context.untracked_files += 1;
            continue;
        }
        if line.starts_with("u ") {
            context.conflicts += 1;
            continue;
        }
        if line.starts_with("1 ") || line.starts_with("2 ") {
            // Porcelain v2 ordinary and renamed records both put XY after the
            // leading record type and space.
            if let Some(xy) = line.get(2..4) {
                let mut chars = xy.chars();
                let index_status = chars.next().unwrap_or('.');
                let worktree_status = chars.next().unwrap_or('.');
                if index_status != '.' {
                    context.staged_files += 1;
                }
                if worktree_status != '.' {
                    context.modified_files += 1;
                }
                if matches!(index_status, 'U') || matches!(worktree_status, 'U') {
                    context.conflicts += 1;
                }
            }
        }
    }
    context
}

fn sanitize_remote(remote: &str) -> String {
    let Some(scheme_end) = remote.find("://") else {
        return remote.to_owned();
    };
    let authority_start = scheme_end + 3;
    let Some(relative_at) = remote[authority_start..].find('@') else {
        return remote.to_owned();
    };
    let at = authority_start + relative_at;
    let slash = remote[authority_start..]
        .find('/')
        .map_or(remote.len(), |relative| authority_start + relative);
    if at > slash {
        return remote.to_owned();
    }
    format!("{}{}", &remote[..authority_start], &remote[at + 1..])
}

#[derive(Debug, Error)]
pub enum GitContextError {
    #[error("could not launch git: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("git could not {operation} (status {status:?}): {stderr}")]
    CommandFailed {
        operation: &'static str,
        status: Option<i32>,
        stderr: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_branch_counts_and_status_without_file_contents() {
        let output = concat!(
            "# branch.oid abcdef\n",
            "# branch.head feature/test\n",
            "# branch.upstream origin/feature/test\n",
            "# branch.ab +2 -3\n",
            "1 .M N... 100644 100644 100644 abc abc src/main.rs\n",
            "1 M. N... 100644 100644 100644 abc abc Cargo.toml\n",
            "? notes.txt\n",
            "u UU N... 100644 100644 100644 100644 a b c file.txt\n",
        );
        let context = parse_porcelain_v2(output, PathBuf::from("/repo"));
        assert_eq!(context.branch.as_deref(), Some("feature/test"));
        assert_eq!(context.ahead, 2);
        assert_eq!(context.behind, 3);
        assert_eq!(context.modified_files, 1);
        assert_eq!(context.staged_files, 1);
        assert_eq!(context.untracked_files, 1);
        assert_eq!(context.conflicts, 1);
    }

    #[test]
    fn strips_credentials_from_https_remote() {
        assert_eq!(
            sanitize_remote("https://alice:secret@example.com/org/repo.git"),
            "https://example.com/org/repo.git"
        );
        assert_eq!(
            sanitize_remote("git@example.com:org/repo.git"),
            "git@example.com:org/repo.git"
        );
    }

    #[test]
    fn non_repository_returns_none() {
        let directory = tempfile::tempdir().unwrap();
        assert!(collect_git_context(directory.path()).unwrap().is_none());
    }

    #[test]
    fn collects_real_local_repository_context() {
        let directory = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(directory.path().join("untracked.txt"), "content").unwrap();

        let context = collect_git_context(directory.path()).unwrap().unwrap();
        assert_eq!(
            context.repo_root,
            fs::canonicalize(directory.path()).unwrap()
        );
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert_eq!(context.untracked_files, 1);
        assert!(context.remote.is_none());
    }
}
