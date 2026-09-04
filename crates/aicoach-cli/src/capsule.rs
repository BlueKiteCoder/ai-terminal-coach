use super::{CapsuleArgs, Paths, atomic_write, ensure_macos};
use aicoach_core::{Config, PrivacyRedactor, strip_terminal_sequences};
use aicoach_ipc::{
    ClientCapabilities, ClientKind, ContextParams, HelloParams, IpcClient, PROTOCOL_VERSION,
    Request, RequestBody, ResponseOutcome, ResponseResult, SessionContext, SessionId,
};
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use std::{
    env, fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

pub(super) fn export(paths: &Paths, args: &CapsuleArgs) -> Result<()> {
    ensure_macos()?;
    let session_id = resolve_capsule_session(paths, &args.session)?;
    let context = fetch_session_context(&paths.socket, session_id, args.last)?;
    let config = Config::load_from(&paths.config)
        .with_context(|| format!("load {}", paths.config.display()))?;

    // Capsules are designed to leave the machine. Redaction therefore stays
    // mandatory even when a user has explicitly disabled provider redaction.
    let mut privacy = config.privacy.clone();
    privacy.redaction = true;
    let redactor = PrivacyRedactor::new(&privacy).context("prepare capsule redaction")?;
    let generated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let report = render_session_capsule(
        &context,
        &paths.home,
        &redactor,
        args.failed_only,
        config.coach.language == "zh-CN",
        &generated_at,
    );

    let mut delivered = false;
    if let Some(output) = args.output.as_ref() {
        let output = if output.is_absolute() {
            output.clone()
        } else {
            env::current_dir()
                .context("resolve capsule output directory")?
                .join(output)
        };
        atomic_write(&output, &report, 0o600)?;
        eprintln!("Capsule written to {}", output.display());
        delivered = true;
    }
    if args.copy {
        copy_to_clipboard(&report)?;
        eprintln!("Capsule copied to the clipboard");
        delivered = true;
    }
    if !delivered {
        print!("{report}");
    }
    Ok(())
}

fn resolve_capsule_session(paths: &Paths, requested: &str) -> Result<SessionId> {
    let value = if requested.trim().is_empty() {
        fs::read_to_string(paths.run_dir.join("active-session")).with_context(
            || "no active terminal session; open a new Zsh prompt or pass --session <UUID>",
        )?
    } else {
        requested.to_owned()
    };
    value
        .trim()
        .parse()
        .with_context(|| format!("invalid session UUID `{}`", value.trim()))
}

pub(super) fn parse_capsule_limit(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "must be a whole number between 1 and 1000".to_owned())?;
    if (1..=1_000).contains(&parsed) {
        Ok(parsed)
    } else {
        Err("must be between 1 and 1000".to_owned())
    }
}

fn fetch_session_context(
    socket: &Path,
    session_id: SessionId,
    max_commands: usize,
) -> Result<SessionContext> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create capsule IPC runtime")?;
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
                        client_name: "aicoach-capsule".to_owned(),
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
                Request::new(
                    Some(session_id),
                    RequestBody::Context(ContextParams {
                        max_commands: Some(max_commands),
                    }),
                ),
                timeout,
            )
            .await
            .context("request terminal session context")?;
        client.close().await.ok();
        match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::Context(context),
            } => Ok(context),
            ResponseOutcome::Error { error } => {
                bail!("daemon could not export session: {}", error.message)
            }
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected daemon context response: {other:?}")
            }
        }
    })
}

fn render_session_capsule(
    context: &SessionContext,
    home: &Path,
    redactor: &PrivacyRedactor,
    failed_only: bool,
    chinese: bool,
    generated_at: &str,
) -> String {
    use std::fmt::Write as _;

    let commands = context
        .commands
        .iter()
        .filter(|command| !failed_only || command.exit_code != 0)
        .collect::<Vec<_>>();
    let failures = commands
        .iter()
        .filter(|command| command.exit_code != 0)
        .count();
    let known_duration_ms = commands
        .iter()
        .filter_map(|command| command.duration_ms)
        .fold(0_u64, u64::saturating_add);
    let has_duration = commands.iter().any(|command| command.duration_ms.is_some());
    let cwd = capsule_text(&context.cwd.to_string_lossy(), home, redactor);
    let shell = capsule_text(&context.shell, home, redactor);

    let mut report = String::new();
    if chinese {
        let _ = writeln!(report, "# AI Terminal Coach 会话胶囊\n");
        let _ = writeln!(
            report,
            "> 在本机生成，未调用 AI。常见密钥和用户主目录已脱敏；分享前仍请人工检查。\n"
        );
        let _ = writeln!(report, "## 摘要\n");
        let _ = writeln!(report, "- 生成时间：{}", markdown_inline(generated_at));
        let _ = writeln!(report, "- Shell：{}", markdown_inline(&shell));
        let _ = writeln!(report, "- 当前目录：{}", markdown_inline(&cwd));
        let _ = writeln!(
            report,
            "- 命令：{} 条，其中失败 {} 条",
            commands.len(),
            failures
        );
        if has_duration {
            let _ = writeln!(
                report,
                "- 已记录总耗时：{}",
                format_duration(known_duration_ms)
            );
        }
    } else {
        let _ = writeln!(report, "# AI Terminal Coach Session Capsule\n");
        let _ = writeln!(
            report,
            "> Generated locally without an AI request. Common secrets and the home directory were redacted; review before sharing.\n"
        );
        let _ = writeln!(report, "## Summary\n");
        let _ = writeln!(report, "- Generated: {}", markdown_inline(generated_at));
        let _ = writeln!(report, "- Shell: {}", markdown_inline(&shell));
        let _ = writeln!(report, "- Current directory: {}", markdown_inline(&cwd));
        let _ = writeln!(
            report,
            "- Commands: {}, including {} failed",
            commands.len(),
            failures
        );
        if has_duration {
            let _ = writeln!(
                report,
                "- Recorded duration: {}",
                format_duration(known_duration_ms)
            );
        }
    }

    if !context.environment.is_empty() {
        let _ = writeln!(
            report,
            "\n## {}\n",
            if chinese { "环境" } else { "Environment" }
        );
        for (key, value) in &context.environment {
            let value = capsule_text(value, home, redactor);
            let _ = writeln!(
                report,
                "- {}: {}",
                markdown_inline(key),
                markdown_inline(&value)
            );
        }
    }

    let _ = writeln!(
        report,
        "\n## {}\n",
        if chinese {
            "命令时间线"
        } else {
            "Command timeline"
        }
    );
    if commands.is_empty() {
        let _ = writeln!(
            report,
            "{}",
            if chinese {
                "没有符合条件的已记录命令。"
            } else {
                "No matching recorded commands."
            }
        );
        return report;
    }

    for (index, command) in commands.iter().enumerate() {
        let status = if command.exit_code == 0 {
            if chinese { "成功" } else { "Success" }
        } else if chinese {
            "失败"
        } else {
            "Failed"
        };
        let duration = command.duration_ms.map_or_else(
            || if chinese { "未知" } else { "unknown" }.to_owned(),
            format_duration,
        );
        let _ = writeln!(
            report,
            "### {}. {} · exit {} · {}\n",
            index + 1,
            status,
            command.exit_code,
            duration
        );
        let command_cwd = capsule_text(&command.cwd.to_string_lossy(), home, redactor);
        let _ = writeln!(
            report,
            "{}: {}\n",
            if chinese { "目录" } else { "Directory" },
            markdown_inline(&command_cwd)
        );
        let command_text = capsule_text(&command.command, home, redactor);
        let _ = writeln!(report, "{}\n", markdown_fence("zsh", &command_text));

        if let Some(stdout) = command
            .stdout_summary
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let stdout = capsule_text(stdout, home, redactor);
            let _ = writeln!(
                report,
                "{}:\n\n{}\n",
                if chinese {
                    "标准输出"
                } else {
                    "Standard output"
                },
                markdown_fence("text", &stdout)
            );
        }
        if let Some(stderr) = command
            .stderr_summary
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let stderr = capsule_text(stderr, home, redactor);
            let _ = writeln!(
                report,
                "{}:\n\n{}\n",
                if chinese {
                    "错误输出"
                } else {
                    "Standard error"
                },
                markdown_fence("text", &stderr)
            );
        }
    }
    report
}

fn capsule_text(value: &str, home: &Path, redactor: &PrivacyRedactor) -> String {
    let home = home.to_string_lossy();
    let home_hidden = if home.is_empty() || home == "/" {
        value.to_owned()
    } else {
        value.replace(home.as_ref(), "~")
    };
    strip_terminal_sequences(&redactor.redact(&home_hidden), true)
}

fn markdown_fence(language: &str, value: &str) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(3_usize.max(longest.saturating_add(1)));
    let value = value.trim_end_matches('\n');
    format!("{fence}{language}\n{value}\n{fence}")
}

fn markdown_inline(value: &str) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(1_usize.max(longest.saturating_add(1)));
    format!("{fence} {value} {fence}")
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds} ms")
    } else if milliseconds < 60_000 {
        format_hundredths(milliseconds.saturating_add(5) / 10, "s")
    } else {
        format_hundredths(milliseconds.saturating_add(300) / 600, "min")
    }
}

fn format_hundredths(value: u64, unit: &str) -> String {
    format!("{}.{:02} {unit}", value / 100, value % 100)
}

fn copy_to_clipboard(contents: &str) -> Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("start macOS clipboard service")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("macOS clipboard stdin is unavailable"))?
        .write_all(contents.as_bytes())
        .context("write capsule to macOS clipboard")?;
    let status = child.wait().context("wait for macOS clipboard service")?;
    if !status.success() {
        bail!("macOS clipboard service exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        path::PathBuf,
        thread,
    };

    #[test]
    fn capsule_is_share_ready_redacted_and_terminal_safe() {
        let home = Path::new("/Users/alice");
        let secret = "capsule-test-secret-abcdefghijklmnopqrstuvwxyz";
        let context = SessionContext {
            session_id: SessionId::new(),
            tty: "/dev/ttys001".to_owned(),
            cwd: home.join("work/demo"),
            shell: "zsh".to_owned(),
            environment: std::collections::BTreeMap::from([(
                "VIRTUAL_ENV".to_owned(),
                "/Users/alice/work/demo/.venv".to_owned(),
            )]),
            commands: vec![aicoach_ipc::ContextCommand {
                command_id: aicoach_ipc::CommandId::new(),
                command: format!("curl 'https://example.test?api_key={secret}'"),
                cwd: home.join("work/demo"),
                exit_code: 22,
                duration_ms: Some(1_250),
                stdout_summary: None,
                stderr_summary: Some(
                    "\u{1b}[31mrequest failed\u{1b}[0m\n```untrusted fence```".to_owned(),
                ),
            }],
        };

        let report = render_session_capsule(
            &context,
            home,
            &PrivacyRedactor::default(),
            false,
            false,
            "2026-09-04T00:00:00Z",
        );

        assert!(report.contains("# AI Terminal Coach Session Capsule"));
        assert!(report.contains("~/work/demo"));
        assert!(report.contains("[REDACTED]"));
        assert!(report.contains("````text"));
        assert!(!report.contains(secret));
        assert!(!report.contains("/Users/alice"));
        assert!(!report.contains('\u{1b}'));
    }

    #[test]
    fn capsule_can_focus_on_failures_and_use_chinese_headings() {
        let context = SessionContext {
            session_id: SessionId::new(),
            tty: "/dev/ttys001".to_owned(),
            cwd: PathBuf::from("/tmp"),
            shell: "zsh".to_owned(),
            environment: std::collections::BTreeMap::new(),
            commands: vec![
                aicoach_ipc::ContextCommand {
                    command_id: aicoach_ipc::CommandId::new(),
                    command: "echo ok".to_owned(),
                    cwd: PathBuf::from("/tmp"),
                    exit_code: 0,
                    duration_ms: Some(4),
                    stdout_summary: Some("ok".to_owned()),
                    stderr_summary: None,
                },
                aicoach_ipc::ContextCommand {
                    command_id: aicoach_ipc::CommandId::new(),
                    command: "false".to_owned(),
                    cwd: PathBuf::from("/tmp"),
                    exit_code: 1,
                    duration_ms: Some(5),
                    stdout_summary: None,
                    stderr_summary: None,
                },
            ],
        };

        let report = render_session_capsule(
            &context,
            Path::new("/Users/alice"),
            &PrivacyRedactor::default(),
            true,
            true,
            "2026-09-04T00:00:00Z",
        );

        assert!(report.contains("# AI Terminal Coach 会话胶囊"));
        assert!(report.contains("命令：1 条，其中失败 1 条"));
        assert!(report.contains("false"));
        assert!(!report.contains("echo ok"));
    }

    #[test]
    fn capsule_fetches_context_through_the_daemon_protocol() {
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("capsule.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let session_id = SessionId::new();
        let expected = SessionContext {
            session_id,
            tty: "/dev/ttys001".to_owned(),
            cwd: PathBuf::from("/tmp/demo"),
            shell: "zsh".to_owned(),
            environment: std::collections::BTreeMap::new(),
            commands: Vec::new(),
        };
        let server_context = expected.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for index in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let aicoach_ipc::Message::Request { request } =
                    serde_json::from_str(&line).unwrap()
                else {
                    panic!("expected request")
                };
                let result = match (&request.body, index) {
                    (RequestBody::Hello(_), 0) => ResponseResult::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: "test".to_owned(),
                    },
                    (RequestBody::Context(params), 1) => {
                        assert_eq!(request.session_id, Some(session_id));
                        assert_eq!(params.max_commands, Some(7));
                        ResponseResult::Context(server_context.clone())
                    }
                    _ => panic!("unexpected request order"),
                };
                let response = aicoach_ipc::Response::ok(&request, result);
                writeln!(
                    stream,
                    "{}",
                    serde_json::to_string(&aicoach_ipc::Message::from(response)).unwrap()
                )
                .unwrap();
            }
        });

        let received = fetch_session_context(&socket, session_id, 7).unwrap();
        server.join().unwrap();
        assert_eq!(received, expected);
    }
}
