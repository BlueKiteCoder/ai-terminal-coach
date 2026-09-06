use super::{AirlockAction, AirlockArgs, Paths, capsule, ensure_macos};
use aicoach_ipc::{
    AirlockOperation, AirlockParams, AirlockStatus, ClientCapabilities, ClientKind, HelloParams,
    IpcClient, PROTOCOL_VERSION, Request, RequestBody, ResponseOutcome, ResponseResult, SessionId,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{env, path::Path, time::Duration};

#[derive(Debug, Serialize)]
struct AirlockStatusReport {
    session_id: SessionId,
    provider_access_enabled: bool,
    cancelled_requests: u64,
    scope: &'static str,
    persistence: &'static str,
    restart_behavior: &'static str,
}

pub(super) fn run(paths: &Paths, args: &AirlockArgs) -> Result<()> {
    ensure_macos()?;
    let session_id = capsule::resolve_capsule_session(paths, &args.session)?;
    let (operation, json) = match args.action.as_ref() {
        Some(AirlockAction::Status(output)) => (AirlockOperation::Status, output.json),
        Some(AirlockAction::Seal) => (AirlockOperation::Seal, false),
        Some(AirlockAction::Open) => (AirlockOperation::Open, false),
        None => (AirlockOperation::Status, false),
    };
    let status = request_airlock(&paths.socket, session_id, operation)?;
    if json {
        let report = AirlockStatusReport {
            session_id,
            provider_access_enabled: status.provider_access_enabled,
            cancelled_requests: status.cancelled_requests,
            scope: "current_session",
            persistence: "daemon_memory_only",
            restart_behavior: "opens_by_default",
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if status.provider_access_enabled {
        println!("Session Airlock: OPEN (provider access allowed)");
        if matches!(operation, AirlockOperation::Open) {
            println!("Opening permits future requests; it does not send one by itself.");
        } else {
            println!("Future completion, analysis, and chat requests may reach the provider.");
        }
    } else {
        println!("Session Airlock: SEALED (local-only)");
        println!("New provider requests are blocked for this session.");
        if status.cancelled_requests > 0 {
            println!(
                "Cancelled active AI requests: {}",
                status.cancelled_requests
            );
        }
    }
    println!("Scope: this session only; state resets to OPEN when the daemon restarts.");
    Ok(())
}

fn request_airlock(
    socket: &Path,
    session_id: SessionId,
    operation: AirlockOperation,
) -> Result<AirlockStatus> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create Airlock IPC runtime")?;
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
                        client_name: "aicoach-airlock".to_owned(),
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
                    RequestBody::Airlock(AirlockParams { operation }),
                ),
                timeout,
            )
            .await
            .context("inspect or change Session Airlock")?;
        client.close().await.ok();
        match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::Airlock(status),
            } => Ok(status),
            ResponseOutcome::Error { error } => {
                bail!("Airlock operation failed: {}", error.message)
            }
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected daemon Airlock response: {other:?}")
            }
        }
    })
}
