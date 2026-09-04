use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aicoach_ai::{
    AiError, AiOperation, AiProvider, AiResult, ChatRequest, ChatResponse, ChatStream,
    CommandCompletionRequest,
};
use aicoach_core::{
    AnalysisCategory, AnalysisCoverage, AnalysisInput, AnalysisResult,
    CompletionOperation as CoreCompletionOperation, CompletionResult as CoreCompletionResult,
    RiskLevel, Severity as CoreSeverity,
};
use aicoach_daemon::{Daemon, DaemonOptions};
use aicoach_ipc::{
    CancelParams, ChatParams, ClientCapabilities, ClientKind, CommandFinishedParams, CommandId,
    CommandStartedParams, CompletionParams, ContextParams, EventBody, HelloParams,
    InsertBufferParams, InsertMode, IpcClient, PROTOCOL_VERSION, RegisterSessionParams, Request,
    RequestBody, Response, ResponseOutcome, ResponseResult, RiskLensParams, SafetyClassification,
    SessionId,
};
use async_trait::async_trait;
use futures_util::stream;
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct TestProvider {
    analysis_calls: AtomicUsize,
    completion_calls: AtomicUsize,
    interrupt_stream: bool,
}

#[async_trait]
impl AiProvider for TestProvider {
    async fn chat(
        &self,
        _request: ChatRequest,
        cancellation: CancellationToken,
    ) -> AiResult<ChatResponse> {
        cancellation.cancelled().await;
        Err(AiError::Cancelled {
            operation: AiOperation::Chat,
        })
    }

    async fn stream_chat(
        &self,
        _request: ChatRequest,
        _cancellation: CancellationToken,
    ) -> AiResult<ChatStream> {
        if self.interrupt_stream {
            Ok(Box::pin(stream::iter([
                Ok("partial".to_owned()),
                Err(AiError::Transport {
                    operation: AiOperation::Chat,
                }),
            ])))
        } else {
            Ok(Box::pin(stream::iter([
                Ok("hello ".to_owned()),
                Ok("world".to_owned()),
            ])))
        }
    }

    async fn analyze_command(
        &self,
        _request: AnalysisInput,
        _cancellation: CancellationToken,
    ) -> AiResult<AnalysisResult> {
        self.analysis_calls.fetch_add(1, Ordering::SeqCst);
        Ok(AnalysisResult {
            need_response: true,
            severity: CoreSeverity::Error,
            category: AnalysisCategory::Unknown,
            title: "Test analysis".to_owned(),
            message: "provider analyzed failure".to_owned(),
            suggested_command: None,
            confidence: 1.0,
        })
    }

    async fn complete_command(
        &self,
        request: CommandCompletionRequest,
        cancellation: CancellationToken,
    ) -> AiResult<CoreCompletionResult> {
        self.completion_calls.fetch_add(1, Ordering::SeqCst);
        if request.buffer == "wait" {
            cancellation.cancelled().await;
            return Err(AiError::Cancelled {
                operation: AiOperation::Completion,
            });
        }
        if request.buffer == "git reset " {
            return Ok(CoreCompletionResult {
                operation: CoreCompletionOperation::Insert,
                command: "--hard".to_owned(),
                cursor: request.buffer.chars().count() + 6,
                description: "complete the reset mode".to_owned(),
            });
        }
        Ok(CoreCompletionResult {
            operation: CoreCompletionOperation::Replace,
            command: format!("{}-done", request.buffer),
            cursor: request.buffer.chars().count() + 5,
            description: "test completion".to_owned(),
        })
    }
}

struct RunningDaemon {
    _directory: TempDir,
    socket: PathBuf,
    daemon: Arc<Daemon>,
    task: JoinHandle<()>,
}

impl RunningDaemon {
    async fn start(provider: Arc<TestProvider>) -> Self {
        Self::start_with_inline_hints(provider, true).await
    }

    async fn start_with_inline_hints(provider: Arc<TestProvider>, inline_hint: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coach.sock");
        let daemon = Daemon::new(
            provider,
            DaemonOptions {
                capture_screen_tail: false,
                inline_hint,
                active_state_dir: Some(directory.path().to_owned()),
                ..DaemonOptions::default()
            },
        );
        let server = Arc::clone(&daemon);
        let server_socket = socket.clone();
        let task = tokio::spawn(async move {
            server.serve_path(server_socket, false).await.unwrap();
        });
        wait_for_socket(&socket).await;
        Self {
            _directory: directory,
            socket,
            daemon,
            task,
        }
    }

    async fn stop(self) {
        self.daemon.request_shutdown();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn wait_for_socket(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("socket did not appear");
}

async fn register(client: &IpcClient, requested: Option<SessionId>, tty: &str) -> SessionId {
    let hello = client
        .send_request(
            None,
            RequestBody::Hello(HelloParams {
                protocol_version: PROTOCOL_VERSION,
                client_name: "integration-test".to_owned(),
                client_version: "1".to_owned(),
                client_kind: ClientKind::Shell,
                capabilities: ClientCapabilities {
                    insert_buffer: true,
                    ..ClientCapabilities::default()
                },
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        hello.outcome,
        ResponseOutcome::Ok {
            result: ResponseResult::Hello { .. }
        }
    ));
    let response = client
        .send_request(
            None,
            RequestBody::RegisterSession(RegisterSessionParams {
                requested_session_id: requested,
                tty: tty.to_owned(),
                pid: Some(std::process::id()),
                cwd: PathBuf::from("/tmp"),
                shell: "zsh".to_owned(),
                terminal: None,
                environment: BTreeMap::new(),
            }),
        )
        .await
        .unwrap();
    let ResponseOutcome::Ok {
        result: ResponseResult::SessionRegistered { session_id },
    } = response.outcome
    else {
        panic!("expected registration response")
    };
    session_id
}

#[tokio::test]
async fn connects_disconnects_and_accepts_a_new_client() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let first = IpcClient::connect(&running.socket).await.unwrap();
    let first_session = register(&first, None, "/dev/ttys001").await;
    first.close().await.unwrap();

    let second = IpcClient::connect(&running.socket).await.unwrap();
    let second_session = register(&second, Some(first_session), "/dev/ttys001").await;
    assert_eq!(first_session, second_session);
    second.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn multiple_sessions_keep_context_isolated() {
    let provider = Arc::new(TestProvider::default());
    let running = RunningDaemon::start(Arc::clone(&provider)).await;
    let first = IpcClient::connect(&running.socket).await.unwrap();
    let second = IpcClient::connect(&running.socket).await.unwrap();
    let first_id = register(&first, None, "/dev/ttys001").await;
    let second_id = register(&second, None, "/dev/ttys002").await;
    let command_id = CommandId::new();
    first
        .send_request(
            Some(first_id),
            RequestBody::CommandStarted(CommandStartedParams {
                command_id,
                command: "echo first".to_owned(),
                cwd: PathBuf::from("/tmp/first"),
                started_at_unix_ms: None,
            }),
        )
        .await
        .unwrap();
    first
        .send_request(
            Some(first_id),
            RequestBody::CommandFinished(CommandFinishedParams {
                command_id,
                command: None,
                cwd: None,
                exit_code: 0,
                stdout: Some("first output".to_owned()),
                stderr: None,
                duration_ms: Some(1),
                environment: BTreeMap::new(),
            }),
        )
        .await
        .unwrap();

    let first_context = first
        .send_request(
            Some(first_id),
            RequestBody::Context(ContextParams::default()),
        )
        .await
        .unwrap();
    let second_context = second
        .send_request(
            Some(second_id),
            RequestBody::Context(ContextParams::default()),
        )
        .await
        .unwrap();
    let command_count = |response: Response| match response.outcome {
        ResponseOutcome::Ok {
            result: ResponseResult::Context(context),
        } => context.commands.len(),
        _ => panic!("expected context response"),
    };
    assert_eq!(command_count(first_context), 1);
    assert_eq!(command_count(second_context), 0);
    assert_eq!(provider.analysis_calls.load(Ordering::SeqCst), 0);
    first.close().await.unwrap();
    second.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn cancel_interrupts_active_completion_and_daemon_remains_responsive() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let client = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&client, None, "/dev/ttys003").await;
    let mut events = client.subscribe();
    let completion = Request::new(
        Some(session),
        RequestBody::Completion(CompletionParams {
            buffer: "wait".to_owned(),
            cursor: 4,
            cwd: PathBuf::from("/tmp"),
        }),
    );
    let completion_id = completion.request_id;
    let pending_client = client.clone();
    let pending = tokio::spawn(async move { pending_client.send(completion).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cancelled = client
        .send_request(
            Some(session),
            RequestBody::Cancel(CancelParams {
                target_request_id: completion_id,
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        cancelled.outcome,
        ResponseOutcome::Ok {
            result: ResponseResult::Accepted
        }
    ));
    let completion_response = tokio::time::timeout(Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        completion_response.outcome,
        ResponseOutcome::Error { ref error } if error.code == "cancelled"
    ));
    let cancelled_event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled_event.request_id, Some(completion_id));
    assert!(matches!(cancelled_event.body, EventBody::RequestCancelled));
    let pong = client
        .send_request(Some(session), RequestBody::Ping)
        .await
        .unwrap();
    assert!(matches!(
        pong.outcome,
        ResponseOutcome::Ok {
            result: ResponseResult::Pong { .. }
        }
    ));
    client.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn ai_insert_cannot_hide_a_dangerous_composed_command() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let client = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&client, None, "/dev/ttys008").await;
    let mut events = client.subscribe();

    let response = client
        .send_request(
            Some(session),
            RequestBody::Completion(CompletionParams {
                buffer: "git reset ".to_owned(),
                cursor: 10,
                cwd: PathBuf::from("/tmp"),
            }),
        )
        .await
        .unwrap();
    let ResponseOutcome::Ok {
        result: ResponseResult::Completion(completion),
    } = response.outcome
    else {
        panic!("expected completion response")
    };
    assert_eq!(
        completion.operation,
        aicoach_ipc::CompletionOperation::Suggest
    );
    assert_eq!(completion.command, "git reset --hard");
    assert!(
        completion
            .description
            .as_deref()
            .is_some_and(|description| description.contains("Local risk: HIGH"))
    );

    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    let EventBody::Hint(hint) = event.body else {
        panic!("expected risk warning")
    };
    assert_eq!(hint.severity, aicoach_ipc::Severity::Error);
    assert_eq!(hint.suggested_command.as_deref(), Some("git reset --hard"));

    client.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn daemon_reclassifies_inserted_commands_before_shell_delivery() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let shell = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&shell, None, "/dev/ttys010").await;
    let mut events = shell.subscribe();

    let response = shell
        .send_request(
            Some(session),
            RequestBody::InsertBuffer(InsertBufferParams {
                command: "rm -rf /".to_owned(),
                cursor: None,
                mode: InsertMode::Replace,
                // A client cannot downgrade the label attached to the shell event.
                safety: Some(SafetyClassification {
                    level: Some(RiskLevel::Low),
                    coverage: AnalysisCoverage::Recognized,
                    safety_rules_enabled: false,
                }),
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        response.outcome,
        ResponseOutcome::Ok {
            result: ResponseResult::Accepted
        }
    ));

    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    let EventBody::InsertBuffer(insert) = event.body else {
        panic!("expected shell-buffer insertion event")
    };
    let safety = insert.safety.expect("daemon safety classification");
    assert_eq!(safety.level, Some(RiskLevel::Critical));
    assert_eq!(safety.coverage, AnalysisCoverage::Recognized);
    assert!(safety.safety_rules_enabled);

    shell.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn risk_lens_is_structured_local_and_provider_free() {
    let provider = Arc::new(TestProvider::default());
    let running = RunningDaemon::start(Arc::clone(&provider)).await;
    let client = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&client, None, "/dev/ttys009").await;

    let response = client
        .send_request(
            Some(session),
            RequestBody::RiskLens(RiskLensParams {
                buffer: "sudo rm -rf ~/Downloads/cache".to_owned(),
                cwd: PathBuf::from("/tmp"),
            }),
        )
        .await
        .unwrap();
    let ResponseOutcome::Ok {
        result: ResponseResult::RiskLens(result),
    } = response.outcome
    else {
        panic!("expected risk lens response")
    };
    assert_eq!(result.report.level, Some(aicoach_core::RiskLevel::High));
    assert_eq!(
        result.report.privilege,
        aicoach_core::PrivilegeRequirement::Administrator
    );
    assert_eq!(
        result.report.recovery,
        aicoach_core::RecoveryProspect::Irreversible
    );
    assert!(result.message.contains("Risk Lens · HIGH"));
    assert!(result.message.contains("~/Downloads/cache"));
    assert_eq!(result.source_cards.len(), 1);
    assert_eq!(result.source_cards[0].reference, "man rm");
    assert!(result.message.contains("Local source: man rm · -R"));
    assert!(result.message.contains("Inference boundary:"));
    assert_eq!(provider.analysis_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.completion_calls.load(Ordering::SeqCst), 0);

    client.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn failed_command_uses_ai_after_local_trigger_and_pushes_hint() {
    let provider = Arc::new(TestProvider::default());
    let running = RunningDaemon::start(Arc::clone(&provider)).await;
    let shell = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&shell, None, "/dev/ttys006").await;
    let mut events = shell.subscribe();
    let command_id = CommandId::new();
    shell
        .send_request(
            Some(session),
            RequestBody::CommandStarted(CommandStartedParams {
                command_id,
                command: "mystery-command --flag".to_owned(),
                cwd: PathBuf::from("/tmp"),
                started_at_unix_ms: None,
            }),
        )
        .await
        .unwrap();
    shell
        .send_request(
            Some(session),
            RequestBody::CommandFinished(CommandFinishedParams {
                command_id,
                command: None,
                cwd: None,
                exit_code: 1,
                stdout: None,
                stderr: Some("unclassified failure".to_owned()),
                duration_ms: Some(1),
                environment: BTreeMap::new(),
            }),
        )
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    let EventBody::Hint(hint) = event.body else {
        panic!("expected hint")
    };
    assert_eq!(hint.message, "provider analyzed failure");
    assert_eq!(provider.analysis_calls.load(Ordering::SeqCst), 1);
    shell.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn streaming_chat_events_return_to_tui_origin_not_shell_owner() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let shell = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&shell, None, "/dev/ttys004").await;
    let tui = IpcClient::connect(&running.socket).await.unwrap();
    let mut tui_events = tui.subscribe();
    let mut shell_events = shell.subscribe();
    let request = Request::new(
        Some(session),
        RequestBody::Chat(ChatParams {
            message: "hello".to_owned(),
            stream: true,
            cwd: None,
            buffer: None,
        }),
    );
    let request_id = request.request_id;
    let accepted = tui.send(request).await.unwrap();
    assert!(matches!(
        accepted.outcome,
        ResponseOutcome::Ok {
            result: ResponseResult::Accepted
        }
    ));
    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), tui_events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.request_id, Some(request_id));
        match event.body {
            EventBody::ChatDelta { delta } => text.push_str(&delta),
            EventBody::ChatDone => break,
            EventBody::ChatFailed { message, .. } => panic!("stream failed: {message}"),
            _ => {}
        }
    }
    assert_eq!(text, "hello world");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shell_events.recv())
            .await
            .is_err()
    );
    shell.close().await.unwrap();
    tui.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn tui_created_session_cannot_claim_shell_buffer_ownership() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let tui = IpcClient::connect(&running.socket).await.unwrap();
    tui.send_request(
        None,
        RequestBody::Hello(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client_name: "standalone-tui".to_owned(),
            client_version: "1".to_owned(),
            client_kind: ClientKind::Tui,
            capabilities: ClientCapabilities {
                insert_buffer: true,
                ..ClientCapabilities::default()
            },
        }),
    )
    .await
    .unwrap();
    let registered = tui
        .send_request(
            None,
            RequestBody::RegisterSession(RegisterSessionParams {
                requested_session_id: None,
                tty: "/dev/tty".to_owned(),
                pid: Some(std::process::id()),
                cwd: PathBuf::from("/tmp"),
                shell: "zsh".to_owned(),
                terminal: None,
                environment: BTreeMap::new(),
            }),
        )
        .await
        .unwrap();
    let ResponseOutcome::Ok {
        result: ResponseResult::SessionRegistered { session_id },
    } = registered.outcome
    else {
        panic!("expected detached TUI session")
    };
    let insert = tui
        .send_request(
            Some(session_id),
            RequestBody::InsertBuffer(aicoach_ipc::InsertBufferParams {
                command: "echo safe".to_owned(),
                cursor: None,
                mode: aicoach_ipc::InsertMode::Replace,
                safety: None,
            }),
        )
        .await
        .unwrap();
    let ResponseOutcome::Error { error } = insert.outcome else {
        panic!("detached TUI must not report a successful shell insertion")
    };
    assert_eq!(error.code, "session_disconnected");
    tui.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn interrupted_stream_reports_failure_instead_of_done() {
    let running = RunningDaemon::start(Arc::new(TestProvider {
        interrupt_stream: true,
        ..TestProvider::default()
    }))
    .await;
    let shell = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&shell, None, "/dev/ttys006").await;
    let tui = IpcClient::connect(&running.socket).await.unwrap();
    tui.send_request(
        None,
        RequestBody::Hello(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client_name: "stream-test-tui".to_owned(),
            client_version: "1".to_owned(),
            client_kind: ClientKind::Tui,
            capabilities: ClientCapabilities::default(),
        }),
    )
    .await
    .unwrap();
    let mut events = tui.subscribe();
    let response = tui
        .send_request(
            Some(session),
            RequestBody::Chat(ChatParams {
                message: "test interruption".to_owned(),
                stream: true,
                cwd: None,
                buffer: None,
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        response.outcome,
        ResponseOutcome::Ok {
            result: ResponseResult::Accepted
        }
    ));
    let mut saw_partial = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event.body {
            EventBody::ChatDelta { delta } => saw_partial |= delta == "partial",
            EventBody::ChatFailed { retryable, .. } => {
                assert!(retryable);
                break;
            }
            EventBody::ChatDone => panic!("interrupted stream was reported as complete"),
            _ => {}
        }
    }
    assert!(saw_partial);
    shell.close().await.unwrap();
    tui.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn disabling_inline_hints_still_notifies_tui_observers() {
    let running =
        RunningDaemon::start_with_inline_hints(Arc::new(TestProvider::default()), false).await;
    let shell = IpcClient::connect(&running.socket).await.unwrap();
    let session = register(&shell, None, "/dev/ttys007").await;
    let mut shell_events = shell.subscribe();

    let tui = IpcClient::connect(&running.socket).await.unwrap();
    tui.send_request(
        None,
        RequestBody::Hello(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client_name: "hint-test-tui".to_owned(),
            client_version: "1".to_owned(),
            client_kind: ClientKind::Tui,
            capabilities: ClientCapabilities::default(),
        }),
    )
    .await
    .unwrap();
    tui.send_request(
        Some(session),
        RequestBody::Context(ContextParams {
            max_commands: Some(1),
        }),
    )
    .await
    .unwrap();
    let mut tui_events = tui.subscribe();

    shell
        .send_request(
            Some(session),
            RequestBody::CommandStarted(CommandStartedParams {
                command_id: CommandId::new(),
                command: "rm -rf /".to_owned(),
                cwd: PathBuf::from("/tmp"),
                started_at_unix_ms: None,
            }),
        )
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(1), tui_events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event.body, EventBody::Hint(_)));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), shell_events.recv())
            .await
            .is_err()
    );
    shell.close().await.unwrap();
    tui.close().await.unwrap();
    running.stop().await;
}

#[tokio::test]
async fn zsh_tab_protocol_registers_pings_and_runs_risk_lens() {
    let running = RunningDaemon::start(Arc::new(TestProvider::default())).await;
    let stream = UnixStream::connect(&running.socket).await.unwrap();
    let (read, mut write) = tokio::io::split(stream);
    let session = SessionId::new();
    write
        .write_all(format!("ZSH\tREGISTER\t{session}\t/dev/ttys005\t42\t/tmp\n").as_bytes())
        .await
        .unwrap();
    let mut lines = BufReader::new(read).lines();
    let registered = lines.next_line().await.unwrap().unwrap();
    assert!(registered.starts_with(&format!("REGISTERED\t{session}\t")));
    write
        .write_all(format!("ZSH\tPING\t{session}\t\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(lines.next_line().await.unwrap().unwrap(), "PONG");
    let lens_id = Request::new(None, RequestBody::Ping).request_id;
    write
        .write_all(format!("ZSH\tLENS\t{session}\t{lens_id}\t/tmp\tgit reset --hard\n").as_bytes())
        .await
        .unwrap();
    let lens = lines.next_line().await.unwrap().unwrap();
    assert!(lens.starts_with(&format!("LENS\t{session}\t{lens_id}\thigh\t")));
    assert!(
        lens.contains("Risk Lens · HIGH · recognized%0AImpact"),
        "{lens}"
    );
    drop(write);
    drop(lines);
    running.stop().await;
}
