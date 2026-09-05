use super::{CheckpointAction, CheckpointArgs, Paths, capsule, ensure_macos};
use aicoach_ipc::{
    CheckpointOperation, CheckpointParams, ClientCapabilities, ClientKind, HelloParams, IpcClient,
    PROTOCOL_VERSION, Request, RequestBody, ResponseOutcome, ResponseResult, SessionCheckpoint,
    SessionId,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    env,
    io::{self, IsTerminal, Read, Write},
    path::Path,
    time::Duration,
};

#[derive(Serialize)]
struct CheckpointStatusReport<'a> {
    session_id: SessionId,
    checkpoint: Option<&'a SessionCheckpoint>,
    persistence: &'static str,
    included_in_provider_prompts: bool,
}

pub(super) fn run(paths: &Paths, args: &CheckpointArgs) -> Result<()> {
    ensure_macos()?;
    let session_id = capsule::resolve_capsule_session(paths, &args.session)?;
    let (operation, json) = match args.action.as_ref() {
        Some(CheckpointAction::Start { name }) => {
            (CheckpointOperation::Start { name: name.clone() }, false)
        }
        Some(CheckpointAction::Resolve { resolution }) => (
            CheckpointOperation::Resolve {
                resolution: read_resolution(resolution.as_deref())?,
            },
            false,
        ),
        Some(CheckpointAction::Status(output)) => (CheckpointOperation::Status, output.json),
        Some(CheckpointAction::Clear) => (CheckpointOperation::Clear, false),
        None => (CheckpointOperation::Status, false),
    };
    let checkpoint = request_checkpoint(&paths.socket, session_id, operation)?;

    match args.action.as_ref() {
        Some(CheckpointAction::Start { .. }) => {
            let checkpoint = checkpoint.context("daemon did not return the new checkpoint")?;
            println!("Checkpoint started: {}", checkpoint.name);
            println!("Future Capsules will focus on commands recorded after this point.");
        }
        Some(CheckpointAction::Resolve { .. }) => {
            let checkpoint = checkpoint.context("daemon did not return the resolved checkpoint")?;
            println!("Checkpoint resolved: {}", checkpoint.name);
            println!("The redacted resolution will be included in `aicoach capsule` output.");
        }
        Some(CheckpointAction::Clear) => {
            println!("Checkpoint cleared; retained terminal context was not deleted.");
        }
        Some(CheckpointAction::Status(_)) | None => {
            print_status(session_id, checkpoint.as_ref(), json)?;
        }
    }
    Ok(())
}

fn read_resolution(argument: Option<&str>) -> Result<String> {
    if let Some(argument) = argument {
        return Ok(argument.to_owned());
    }
    let mut resolution = String::new();
    if io::stdin().is_terminal() {
        eprint!("Resolution (kept out of Shell history): ");
        io::stderr().flush().context("flush resolution prompt")?;
        io::stdin()
            .read_line(&mut resolution)
            .context("read checkpoint resolution")?;
    } else {
        io::stdin()
            .read_to_string(&mut resolution)
            .context("read checkpoint resolution from stdin")?;
    }
    Ok(resolution)
}

fn request_checkpoint(
    socket: &Path,
    session_id: SessionId,
    operation: CheckpointOperation,
) -> Result<Option<SessionCheckpoint>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create checkpoint IPC runtime")?;
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
                        client_name: "aicoach-checkpoint".to_owned(),
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
                    RequestBody::Checkpoint(CheckpointParams {
                        operation,
                        exclude_active_command: true,
                    }),
                ),
                timeout,
            )
            .await
            .context("update terminal checkpoint")?;
        client.close().await.ok();
        match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::Checkpoint { checkpoint },
            } => Ok(checkpoint.map(|checkpoint| *checkpoint)),
            ResponseOutcome::Error { error } => bail!("checkpoint failed: {}", error.message),
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected daemon checkpoint response: {other:?}")
            }
        }
    })
}

fn print_status(
    session_id: SessionId,
    checkpoint: Option<&SessionCheckpoint>,
    json: bool,
) -> Result<()> {
    let report = CheckpointStatusReport {
        session_id,
        checkpoint,
        persistence: "daemon_memory_only",
        included_in_provider_prompts: false,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let Some(checkpoint) = checkpoint else {
        println!("No active checkpoint for session {session_id}.");
        println!("Checkpoint data is memory-only and is never included in provider prompts.");
        return Ok(());
    };
    println!("Checkpoint: {}", checkpoint.name);
    println!(
        "Status: {}",
        if checkpoint.resolution.is_some() {
            "resolved"
        } else {
            "active"
        }
    );
    println!(
        "Started: {}",
        format_timestamp(checkpoint.started_at_unix_ms)
    );
    if let Some(resolved_at) = checkpoint.resolved_at_unix_ms {
        println!("Resolved: {}", format_timestamp(resolved_at));
    }
    if let Some(resolution) = checkpoint.resolution.as_deref() {
        println!("Resolution:\n{resolution}");
    }
    println!("Storage: daemon memory only; excluded from AI provider prompts.");
    Ok(())
}

fn format_timestamp(timestamp_ms: u64) -> String {
    i64::try_from(timestamp_ms)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map_or_else(|| timestamp_ms.to_string(), |time| time.to_rfc3339())
}
