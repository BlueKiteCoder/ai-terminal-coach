use super::{OutputArgs, Paths, TwinAction, TwinArgs, ensure_macos};
use aicoach_core::{
    Config, SensitiveEnvironmentValue, TwinTerminalCommandSnapshot, TwinTerminalDiffEntry,
    TwinTerminalDiffField, TwinTerminalDiffKind, TwinTerminalDiffValue, TwinTerminalGitSnapshot,
    TwinTerminalSnapshot, strip_terminal_sequences,
};
use aicoach_ipc::{
    ClientCapabilities, ClientKind, HelloParams, IpcClient, PROTOCOL_VERSION, Request, RequestBody,
    ResponseOutcome, ResponseResult, TwinBaselineSummary, TwinDiffOperation, TwinDiffParams,
    TwinDiffResult,
};
use anyhow::{Context, Result, bail};
use rustix::fs::{Mode, OFlags};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const OBSERVED_COMMANDS: [&str; 11] = [
    "brew",
    "cargo",
    "clang",
    "git",
    "node",
    "npm",
    "python",
    "python3",
    "rustc",
    "swift",
    "xcodebuild",
];

// These values often contain private paths or compiler flags. Compare only a
// domain-separated digest, never the original value.
const HASHED_ENVIRONMENT: [&str; 13] = [
    "ARCHFLAGS",
    "CFLAGS",
    "CPATH",
    "CPPFLAGS",
    "DEVELOPER_DIR",
    "DYLD_LIBRARY_PATH",
    "JAVA_HOME",
    "LDFLAGS",
    "LIBRARY_PATH",
    "PATH",
    "PKG_CONFIG_PATH",
    "PYTHONPATH",
    "SDKROOT",
];

const MAX_GIT_METADATA_BYTES: u64 = 4_096;
const MAX_GIT_BRANCH_CHARS: usize = 256;

pub(super) fn run(paths: &Paths, args: &TwinArgs) -> Result<()> {
    ensure_macos()?;
    let operation = match args.action.as_ref() {
        Some(TwinAction::Mark { name }) => TwinDiffOperation::Mark {
            name: name.clone(),
            snapshot: Box::new(collect_snapshot()?),
        },
        Some(TwinAction::Diff { name, .. }) => TwinDiffOperation::Diff {
            name: name.clone(),
            current: Box::new(collect_snapshot()?),
        },
        Some(TwinAction::Clear { name }) => TwinDiffOperation::Clear { name: name.clone() },
        Some(TwinAction::List(_)) | None => TwinDiffOperation::List,
    };
    let result = request_twin_diff(&paths.socket, operation)?;
    let json = matches!(
        args.action.as_ref(),
        Some(TwinAction::Diff { json: true, .. } | TwinAction::List(OutputArgs { json: true }))
    );
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let chinese =
        Config::load_from(&paths.config).is_ok_and(|config| config.coach.language == "zh-CN");
    print_result(&result, chinese);
    Ok(())
}

fn collect_snapshot() -> Result<TwinTerminalSnapshot> {
    let cwd = env::current_dir().context("read current working directory")?;
    let home = dirs::home_dir();
    let path = env::var_os("PATH").unwrap_or_default();
    let raw_commands = OBSERVED_COMMANDS
        .into_iter()
        .map(|name| (name.to_owned(), resolve_executable(name, &path)))
        .collect::<BTreeMap<_, _>>();
    let commands = raw_commands
        .iter()
        .map(|(name, resolved_path)| {
            (
                name.clone(),
                TwinTerminalCommandSnapshot {
                    resolved_path: resolved_path
                        .as_deref()
                        .map(|path| normalize_path(path, home.as_deref())),
                },
            )
        })
        .collect();
    let git = collect_git_snapshot(&cwd, home.as_deref());
    let homebrew_prefix = raw_commands
        .get("brew")
        .and_then(Option::as_deref)
        .and_then(|path| path.parent())
        .and_then(Path::parent)
        .map(|path| normalize_path(path, home.as_deref()));
    let xcode_developer_dir = system_line("/usr/bin/xcode-select", &["-p"])
        .map(PathBuf::from)
        .map(|path| normalize_path(&path, home.as_deref()));
    let sensitive_environment = HASHED_ENVIRONMENT
        .into_iter()
        .filter_map(|name| {
            env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(|value| {
                    (
                        name.to_owned(),
                        SensitiveEnvironmentValue::digest(name, value.as_encoded_bytes()),
                    )
                })
        })
        .collect();

    Ok(TwinTerminalSnapshot {
        cwd: normalize_path(&cwd, home.as_deref()),
        terminal_program: safe_text(env::var("TERM_PROGRAM").ok(), 80),
        // The installed Zsh integration records the calling shell itself.
        // The CLI binary can be a different architecture, so never infer the
        // terminal architecture from this child process.
        shell_architecture: safe_text(env::var("AICOACH_SHELL_ARCH").ok(), 32),
        rosetta_translated: shell_translation(),
        homebrew_prefix,
        xcode_developer_dir,
        virtual_environment: env::var_os("VIRTUAL_ENV")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| normalize_path(&path, home.as_deref())),
        conda_environment: safe_text(env::var("CONDA_DEFAULT_ENV").ok(), 120),
        git,
        commands,
        sensitive_environment,
    })
}

fn resolve_executable(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    env::split_paths(path).find_map(|directory| {
        let candidate = directory.join(name);
        let metadata = fs::metadata(&candidate).ok()?;
        (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

fn normalize_path(path: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home
        && let Ok(relative) = path.strip_prefix(home)
    {
        return if relative.as_os_str().is_empty() {
            PathBuf::from("$HOME")
        } else {
            PathBuf::from("$HOME").join(relative)
        };
    }
    path.to_path_buf()
}

fn system_line(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > 4_096 {
        return None;
    }
    safe_text(String::from_utf8(output.stdout).ok(), 1_024)
}

fn collect_git_snapshot(cwd: &Path, home: Option<&Path>) -> Option<TwinTerminalGitSnapshot> {
    let (repository, git_directory) = find_git_directory(cwd)?;
    let head =
        rustix::fs::openat(&git_directory, "HEAD", regular_file_flags(), Mode::empty()).ok()?;
    let head = read_bounded_regular_file(fs::File::from(head))?;
    let head = std::str::from_utf8(&head).ok()?.trim();
    let (branch, detached) = if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        if branch.is_empty()
            || branch.chars().count() > MAX_GIT_BRANCH_CHARS
            || strip_terminal_sequences(branch, false) != branch
        {
            return None;
        }
        (Some(branch.to_owned()), false)
    } else if matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        (None, true)
    } else {
        return None;
    };
    Some(TwinTerminalGitSnapshot {
        repository: normalize_path(&repository, home),
        branch,
        detached,
    })
}

fn find_git_directory(cwd: &Path) -> Option<(PathBuf, fs::File)> {
    for repository in cwd.ancestors() {
        let repository_directory = open_directory_no_symlinks(repository)?;
        let marker = match rustix::fs::openat(
            &repository_directory,
            ".git",
            regular_file_flags(),
            Mode::empty(),
        ) {
            Ok(marker) => fs::File::from(marker),
            Err(error) if error == rustix::io::Errno::NOENT => continue,
            Err(_) => return None,
        };
        let metadata = marker.metadata().ok()?;
        if metadata.is_dir() {
            return Some((repository.to_path_buf(), marker));
        }
        if !metadata.is_file() {
            return None;
        }
        let pointer = read_bounded_regular_file(marker)?;
        let pointer = std::str::from_utf8(&pointer)
            .ok()?
            .trim()
            .strip_prefix("gitdir: ")?;
        if pointer.is_empty() || strip_terminal_sequences(pointer, false) != pointer {
            return None;
        }
        let pointer = Path::new(pointer);
        let directory = if pointer.is_absolute() {
            open_directory_no_symlinks(pointer)?
        } else {
            open_directory_from(repository_directory, pointer)?
        };
        return Some((repository.to_path_buf(), directory));
    }
    None
}

fn open_directory_no_symlinks(path: &Path) -> Option<fs::File> {
    if !path.is_absolute() {
        return None;
    }
    let directory = fs::File::from(rustix::fs::open("/", directory_flags(), Mode::empty()).ok()?);
    open_directory_from(directory, path)
}

fn open_directory_from(mut directory: fs::File, path: &Path) -> Option<fs::File> {
    for component in path.components() {
        let component = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(component) => component,
            Component::ParentDir => std::ffi::OsStr::new(".."),
            Component::Prefix(_) => return None,
        };
        directory = fs::File::from(
            rustix::fs::openat(&directory, component, directory_flags(), Mode::empty()).ok()?,
        );
    }
    Some(directory)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK
}

fn regular_file_flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK
}

fn read_bounded_regular_file(file: fs::File) -> Option<Vec<u8>> {
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_GIT_METADATA_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
    file.take(MAX_GIT_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_GIT_METADATA_BYTES).then_some(bytes)
}

fn shell_translation() -> Option<bool> {
    match env::var("AICOACH_SHELL_TRANSLATED").ok()?.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn safe_text(value: Option<String>, limit: usize) -> Option<String> {
    let value = value?;
    let value = value
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect::<String>();
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn request_twin_diff(socket: &Path, operation: TwinDiffOperation) -> Result<TwinDiffResult> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create Twin Terminal Diff IPC runtime")?;
    runtime.block_on(async {
        let client = IpcClient::connect(socket).await.with_context(|| {
            format!("connect to {}; run `aicoach start` first", socket.display())
        })?;
        let timeout = Duration::from_secs(2);
        let hello = client
            .send_timeout(
                Request::new(
                    None,
                    RequestBody::Hello(HelloParams {
                        protocol_version: PROTOCOL_VERSION,
                        client_name: "aicoach-twin".to_owned(),
                        client_version: env!("CARGO_PKG_VERSION").to_owned(),
                        client_kind: ClientKind::Cli,
                        capabilities: ClientCapabilities {
                            push_events: false,
                            streaming: false,
                            insert_buffer: false,
                            shell_line_protocol: false,
                        },
                    }),
                ),
                timeout,
            )
            .await
            .context("handshake with daemon")?;
        match hello.outcome {
            ResponseOutcome::Ok {
                result:
                    ResponseResult::Hello {
                        protocol_version, ..
                    },
            } if protocol_version == PROTOCOL_VERSION => {}
            ResponseOutcome::Error { error } => {
                bail!("daemon rejected handshake: {}", error.message)
            }
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected daemon handshake response: {other:?}")
            }
        }
        let response = client
            .send_timeout(
                Request::new(None, RequestBody::TwinDiff(TwinDiffParams { operation })),
                timeout,
            )
            .await
            .context("run local Twin Terminal Diff")?;
        client.close().await.ok();
        match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::TwinDiff(result),
            } => Ok(*result),
            ResponseOutcome::Error { error } => {
                bail!("Twin Terminal Diff failed: {}", error.message)
            }
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected Twin Terminal Diff response: {other:?}")
            }
        }
    })
}

fn print_result(result: &TwinDiffResult, chinese: bool) {
    match result {
        TwinDiffResult::Marked { name } => {
            if chinese {
                println!("已标记已知正常终端基线：{name}");
                println!("仅保存在 daemon 内存中；不调用 AI，daemon 重启后自动清除。");
                println!("请在异常终端运行：aicoach twin diff --against {name}");
            } else {
                println!("Known-good terminal baseline marked: {name}");
                println!(
                    "Daemon memory only; no AI call; cleared automatically on daemon restart."
                );
                println!("In the failing terminal, run: aicoach twin diff --against {name}");
            }
        }
        TwinDiffResult::Diff { name, report } => {
            if chinese {
                println!("Twin Terminal Diff · 当前终端 vs {name}");
                println!("本地确定性比较；不调用 AI，敏感环境变量只比较摘要。\n");
            } else {
                println!("Twin Terminal Diff · current terminal vs {name}");
                println!(
                    "Deterministic and local; no AI call; sensitive environment values are digest-only.\n"
                );
            }
            if report.entries.is_empty() {
                println!(
                    "{}",
                    if chinese {
                        "在有界快照中没有发现差异。"
                    } else {
                        "No differences found in the bounded snapshot."
                    }
                );
                return;
            }
            for entry in &report.entries {
                println!("- {}", render_entry(entry, chinese));
            }
        }
        TwinDiffResult::List { baselines } => print_baselines(baselines, chinese),
        TwinDiffResult::Cleared { removed } => {
            if chinese {
                println!("已清除 {removed} 个内存基线。");
            } else {
                println!("Cleared {removed} memory-only baseline(s).");
            }
        }
    }
}

fn print_baselines(baselines: &[TwinBaselineSummary], chinese: bool) {
    if baselines.is_empty() {
        println!(
            "{}",
            if chinese {
                "没有 Twin Terminal 基线。运行 `aicoach twin mark` 创建一个。"
            } else {
                "No Twin Terminal baselines. Run `aicoach twin mark` to create one."
            }
        );
        return;
    }
    println!(
        "{}",
        if chinese {
            "Twin Terminal 内存基线："
        } else {
            "Twin Terminal memory-only baselines:"
        }
    );
    for baseline in baselines {
        println!("- {} ({}s)", baseline.name, baseline.age_ms / 1_000);
    }
}

fn render_entry(entry: &TwinTerminalDiffEntry, chinese: bool) -> String {
    let label = field_label(&entry.field, chinese);
    let before = entry
        .baseline
        .as_ref()
        .map_or_else(|| absent(chinese), |value| render_value(value, chinese));
    let after = entry
        .current
        .as_ref()
        .map_or_else(|| absent(chinese), |value| render_value(value, chinese));
    let change = match (chinese, entry.kind) {
        (true, TwinTerminalDiffKind::Changed) => "变化",
        (true, TwinTerminalDiffKind::OnlyBaseline) => "仅基线存在",
        (true, TwinTerminalDiffKind::OnlyCurrent) => "仅当前存在",
        (false, TwinTerminalDiffKind::Changed) => "changed",
        (false, TwinTerminalDiffKind::OnlyBaseline) => "baseline only",
        (false, TwinTerminalDiffKind::OnlyCurrent) => "current only",
    };
    format!("{label} [{change}]: {before} → {after}")
}

fn field_label(field: &TwinTerminalDiffField, chinese: bool) -> String {
    let label = match field {
        TwinTerminalDiffField::WorkingDirectory => ("工作目录", "working directory"),
        TwinTerminalDiffField::TerminalProgram => ("终端", "terminal"),
        TwinTerminalDiffField::ShellArchitecture => {
            ("调用 Shell 架构", "calling shell architecture")
        }
        TwinTerminalDiffField::RosettaTranslation => ("Rosetta 转译", "Rosetta translation"),
        TwinTerminalDiffField::HomebrewPrefix => ("Homebrew 前缀", "Homebrew prefix"),
        TwinTerminalDiffField::XcodeDeveloperDirectory => {
            ("Xcode 开发目录", "Xcode developer directory")
        }
        TwinTerminalDiffField::VirtualEnvironment => {
            ("Python 虚拟环境", "Python virtual environment")
        }
        TwinTerminalDiffField::CondaEnvironment => ("Conda 环境", "Conda environment"),
        TwinTerminalDiffField::GitRepository => ("Git 仓库", "Git repository"),
        TwinTerminalDiffField::GitBranch => ("Git 分支", "Git branch"),
        TwinTerminalDiffField::GitDetached => ("Git detached HEAD", "Git detached HEAD"),
        TwinTerminalDiffField::Command(name) => {
            return format!("{} `{name}`", if chinese { "命令" } else { "command" });
        }
        TwinTerminalDiffField::SensitiveEnvironment(name) => {
            return format!(
                "{} `{name}`",
                if chinese {
                    "环境摘要"
                } else {
                    "environment digest"
                }
            );
        }
    };
    if chinese { label.0 } else { label.1 }.to_owned()
}

fn render_value(value: &TwinTerminalDiffValue, chinese: bool) -> String {
    match value {
        TwinTerminalDiffValue::Path(path) => format!("`{}`", path.display()),
        TwinTerminalDiffValue::Text(value) => format!("`{value}`"),
        TwinTerminalDiffValue::Boolean(value) => value.to_string(),
        TwinTerminalDiffValue::Command(command) => command
            .resolved_path
            .as_ref()
            .map_or_else(|| absent(chinese), |path| format!("`{}`", path.display())),
        TwinTerminalDiffValue::Sensitive => if chinese {
            "存在（值已隐藏）"
        } else {
            "present (value hidden)"
        }
        .to_owned(),
    }
}

fn absent(chinese: bool) -> String {
    if chinese { "无" } else { "absent" }.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};

    #[test]
    fn home_paths_are_normalized_without_touching_other_paths() {
        assert_eq!(
            normalize_path(
                Path::new("/Users/alice/work"),
                Some(Path::new("/Users/alice"))
            ),
            PathBuf::from("$HOME/work")
        );
        assert_eq!(
            normalize_path(Path::new("/opt/homebrew"), Some(Path::new("/Users/alice"))),
            PathBuf::from("/opt/homebrew")
        );
    }

    #[test]
    fn executable_resolution_respects_path_order_and_execute_bits() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::write(first.path().join("demo"), "not executable").unwrap();
        let executable = second.path().join("demo");
        let _file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o700)
            .open(&executable)
            .unwrap();
        let path = env::join_paths([first.path(), second.path()]).unwrap();
        assert_eq!(resolve_executable("demo", &path), Some(executable));
    }

    #[test]
    fn sensitive_diff_rendering_never_contains_a_digest_or_value() {
        let entry = TwinTerminalDiffEntry {
            field: TwinTerminalDiffField::SensitiveEnvironment("SDKROOT".to_owned()),
            kind: TwinTerminalDiffKind::Changed,
            baseline: Some(TwinTerminalDiffValue::Sensitive),
            current: Some(TwinTerminalDiffValue::Sensitive),
        };
        assert_eq!(
            render_entry(&entry, false),
            "environment digest `SDKROOT` [changed]: present (value hidden) → present (value hidden)"
        );
    }

    #[test]
    fn controls_are_removed_from_observed_labels() {
        assert_eq!(
            safe_text(Some("  Terminal\u{1b}[31m  ".to_owned()), 80),
            Some("Terminal[31m".to_owned())
        );
    }

    #[test]
    fn fixed_system_probe_does_not_inherit_the_calling_environment() {
        assert_eq!(system_line("/usr/bin/env", &[]), None);
    }

    #[test]
    fn git_snapshot_reads_only_bounded_head_metadata() {
        let repository = tempfile::Builder::new()
            .prefix("aicoach-twin-git-")
            .tempdir_in(env::current_dir().unwrap())
            .unwrap();
        let git_directory = repository.path().join(".git");
        fs::create_dir(&git_directory).unwrap();
        fs::write(git_directory.join("HEAD"), "ref: refs/heads/feature/twin\n").unwrap();

        let sentinel = repository.path().join("filter-invoked");
        let filter = repository.path().join("dangerous-filter");
        fs::write(
            &filter,
            format!(
                "#!/bin/sh\n/usr/bin/touch {}\n",
                shell_words::quote(sentinel.to_str().unwrap())
            ),
        )
        .unwrap();
        fs::write(
            git_directory.join("config"),
            format!("[filter \"danger\"]\n\tprocess = {}\n", filter.display()),
        )
        .unwrap();
        fs::write(
            repository.path().join(".gitattributes"),
            "* filter=danger\n",
        )
        .unwrap();

        let snapshot = collect_git_snapshot(repository.path(), None).unwrap();
        assert_eq!(snapshot.repository, repository.path());
        assert_eq!(snapshot.branch.as_deref(), Some("feature/twin"));
        assert!(!snapshot.detached);
        assert!(!sentinel.exists());

        fs::write(
            git_directory.join("HEAD"),
            vec![b'x'; usize::try_from(MAX_GIT_METADATA_BYTES).unwrap() + 1],
        )
        .unwrap();
        assert!(collect_git_snapshot(repository.path(), None).is_none());

        let outside_head = repository.path().join("outside-head");
        fs::write(&outside_head, "ref: refs/heads/secret\n").unwrap();
        fs::remove_file(git_directory.join("HEAD")).unwrap();
        std::os::unix::fs::symlink(&outside_head, git_directory.join("HEAD")).unwrap();
        assert!(collect_git_snapshot(repository.path(), None).is_none());

        fs::remove_file(git_directory.join("HEAD")).unwrap();
        let mkfifo = Command::new("/usr/bin/mkfifo")
            .arg(git_directory.join("HEAD"))
            .status()
            .unwrap();
        assert!(mkfifo.success());
        assert!(collect_git_snapshot(repository.path(), None).is_none());
    }

    #[test]
    fn linked_worktree_git_directory_is_resolved_without_running_git() {
        let root = tempfile::Builder::new()
            .prefix("aicoach-twin-worktree-")
            .tempdir_in(env::current_dir().unwrap())
            .unwrap();
        let repository = root.path().join("worktree");
        let admin = root.path().join("admin");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&admin).unwrap();
        fs::write(repository.join(".git"), "gitdir: ../admin\n").unwrap();
        fs::write(
            admin.join("HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();

        let snapshot = collect_git_snapshot(&repository, None).unwrap();
        assert_eq!(snapshot.repository, repository);
        assert_eq!(snapshot.branch, None);
        assert!(snapshot.detached);

        let nested_admin = repository.join("admin");
        fs::create_dir(&nested_admin).unwrap();
        fs::write(nested_admin.join("HEAD"), "ref: refs/heads/hidden\n").unwrap();
        fs::write(repository.join(".git"), "gitdir: missing/../admin\n").unwrap();
        assert!(collect_git_snapshot(&repository, None).is_none());

        let symlink_target = repository.join("symlink-target");
        fs::create_dir(&symlink_target).unwrap();
        std::os::unix::fs::symlink(&symlink_target, repository.join("symlink")).unwrap();
        fs::write(repository.join(".git"), "gitdir: symlink/../admin\n").unwrap();
        assert!(collect_git_snapshot(&repository, None).is_none());

        fs::write(repository.join(".git"), "gitdir: ../admin\n").unwrap();
        fs::remove_file(admin.join("HEAD")).unwrap();
        fs::remove_dir(&admin).unwrap();
        let redirected = root.path().join("redirected");
        fs::create_dir(&redirected).unwrap();
        fs::write(redirected.join("HEAD"), "ref: refs/heads/secret\n").unwrap();
        std::os::unix::fs::symlink(&redirected, &admin).unwrap();
        assert!(collect_git_snapshot(&repository, None).is_none());
    }
}
