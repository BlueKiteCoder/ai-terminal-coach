use std::{collections::BTreeMap, fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use aicoach_core::{AnalysisCoverage, RiskLensReport, RiskLevel, SourceCard};

pub const PROTOCOL_VERSION: u16 = 2;
pub const DEFAULT_MAX_FRAME_LENGTH: usize = 4 * 1024 * 1024;
pub const SHELL_ENVIRONMENT_ALLOWLIST: [&str; 7] = [
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "COLORTERM",
    "VIRTUAL_ENV",
    "CONDA_DEFAULT_ENV",
];

const MAX_ENVIRONMENT_VALUE_CHARS: usize = 4_096;

/// Retain only explicitly approved, non-secret shell metadata.
///
/// This is deliberately applied at the daemon state boundary as well as by
/// shell clients, so a custom JSON client cannot smuggle arbitrary variables
/// (for example API keys) into retained context or provider prompts.
pub fn sanitize_shell_environment(
    environment: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    environment
        .into_iter()
        .filter(|(key, _)| SHELL_ENVIRONMENT_ALLOWLIST.contains(&key.as_str()))
        .map(|(key, value)| {
            let value = value
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_ENVIRONMENT_VALUE_CHARS)
                .collect();
            (key, value)
        })
        .collect()
}

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(RequestId);
uuid_id!(SessionId);
uuid_id!(CommandId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Shell,
    Tui,
    Cli,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub push_events: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub insert_buffer: bool,
    #[serde(default)]
    pub shell_line_protocol: bool,
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            push_events: true,
            streaming: true,
            insert_buffer: false,
            shell_line_protocol: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol_version: u16,
    pub client_name: String,
    pub client_version: String,
    pub client_kind: ClientKind,
    #[serde(default)]
    pub capabilities: ClientCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterSessionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_session_id: Option<SessionId>,
    pub tty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cwd: PathBuf,
    #[serde(default = "default_shell")]
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusParams {
    pub tty: String,
}

fn default_shell() -> String {
    "zsh".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandStartedParams {
    pub command_id: CommandId,
    pub command: String,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFinishedParams {
    pub command_id: CommandId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionParams {
    pub buffer: String,
    pub cursor: usize,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskLensParams {
    pub buffer: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskLensResult {
    pub report: RiskLensReport,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_cards: Vec<SourceCard>,
    /// Localized, terminal-safe presentation of the structured report.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelParams {
    pub target_request_id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatParams {
    pub message: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_commands: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CheckpointOperation {
    Start { name: String },
    Resolve { resolution: String },
    Status,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointParams {
    #[serde(flatten)]
    pub operation: CheckpointOperation,
    /// CLI calls made from the observed shell can exclude their own in-flight
    /// bookkeeping command from the resulting Capsule interval.
    #[serde(default)]
    pub exclude_active_command: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DataOperation {
    Inventory,
    ClearSession,
    ClearChatHistory,
    ClearFailureMemory,
    ClearAllTransient,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataParams {
    #[serde(flatten)]
    pub operation: DataOperation,
    /// A control command issued from the observed shell should disappear
    /// together with the data it clears when its FINISH frame arrives.
    #[serde(default)]
    pub exclude_active_command: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertMode {
    Replace,
    Insert,
    Suggest,
}

/// A compact, local-only safety verdict attached by the daemon before a
/// command is delivered to a shell buffer. Clients may omit it in requests;
/// the daemon is the authority for the event sent to ZLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyClassification {
    pub level: Option<RiskLevel>,
    pub coverage: AnalysisCoverage,
    pub safety_rules_enabled: bool,
}

impl From<&RiskLensReport> for SafetyClassification {
    fn from(report: &RiskLensReport) -> Self {
        Self {
            level: report.level,
            coverage: report.coverage,
            safety_rules_enabled: report.safety_rules_enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsertBufferParams {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<usize>,
    #[serde(default = "replace_mode")]
    pub mode: InsertMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyClassification>,
}

const fn replace_mode() -> InsertMode {
    InsertMode::Replace
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ShutdownParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RequestBody {
    Hello(HelloParams),
    RegisterSession(RegisterSessionParams),
    Focus(FocusParams),
    CommandStarted(CommandStartedParams),
    CommandFinished(CommandFinishedParams),
    Completion(CompletionParams),
    RiskLens(RiskLensParams),
    Cancel(CancelParams),
    Chat(ChatParams),
    Context(ContextParams),
    Checkpoint(CheckpointParams),
    Data(DataParams),
    InsertBuffer(InsertBufferParams),
    Disconnect,
    Ping,
    Shutdown(ShutdownParams),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(flatten)]
    pub body: RequestBody,
}

impl Request {
    pub fn new(session_id: Option<SessionId>, body: RequestBody) -> Self {
        Self {
            request_id: RequestId::new(),
            session_id,
            body,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOperation {
    Replace,
    Insert,
    Suggest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionResult {
    #[serde(rename = "type", alias = "operation")]
    pub operation: CompletionOperation,
    pub command: String,
    pub cursor: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    pub severity: Severity,
    pub title: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCommand {
    pub command_id: CommandId,
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub name: String,
    pub started_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_after_command_id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_command_id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_after_command_id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_command_id: Option<CommandId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContext {
    pub session_id: SessionId,
    pub tty: String,
    pub cwd: PathBuf,
    pub shell: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Box<SessionCheckpoint>>,
    pub commands: Vec<ContextCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDataSummary {
    pub session_id: SessionId,
    pub connected: bool,
    pub command_records: usize,
    pub chat_messages: usize,
    pub environment_values: usize,
    pub checkpoint_present: bool,
    pub environment_baseline_present: bool,
    pub in_flight_commands: usize,
    pub discarded_finish_markers: usize,
    pub active_ai_requests: usize,
    pub pending_failure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDataLimits {
    pub max_commands_per_session: usize,
    pub max_output_chars_per_command: usize,
    pub max_total_context_chars_per_session: usize,
    pub chat_history_enabled: bool,
    pub max_chat_messages_per_session: usize,
    pub max_sessions: usize,
    pub disconnected_session_ttl_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClearScope {
    Session,
    ChatHistory,
    FailureMemory,
    AllTransient,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DataRemovalSummary {
    pub sessions_affected: usize,
    pub command_records: usize,
    pub chat_messages: usize,
    pub persisted_chat_messages: usize,
    pub failure_fingerprints: usize,
    pub environment_values: usize,
    pub checkpoints: usize,
    pub environment_baselines: usize,
    pub in_flight_commands: usize,
    pub active_ai_requests: usize,
    pub pending_failures: usize,
    pub source_card_cache_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum DaemonDataResult {
    Inventory {
        sessions: Vec<SessionDataSummary>,
        source_card_cache_entries: usize,
        limits: SessionDataLimits,
    },
    Cleared {
        scope: DataClearScope,
        removed: DataRemovalSummary,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum ResponseResult {
    Hello {
        protocol_version: u16,
        server_version: String,
    },
    SessionRegistered {
        session_id: SessionId,
    },
    Accepted,
    Completion(CompletionResult),
    RiskLens(RiskLensResult),
    Chat {
        message: String,
    },
    Context(SessionContext),
    Checkpoint {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<Box<SessionCheckpoint>>,
    },
    Data(Box<DaemonDataResult>),
    Pong {
        unix_ms: u64,
    },
    ShutdownAccepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseOutcome {
    Ok {
        #[serde(flatten)]
        result: ResponseResult,
    },
    Error {
        error: ProtocolError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(flatten)]
    pub outcome: ResponseOutcome,
}

impl Response {
    pub fn ok(request: &Request, result: ResponseResult) -> Self {
        Self {
            request_id: request.request_id,
            session_id: request.session_id,
            outcome: ResponseOutcome::Ok { result },
        }
    }

    pub fn error(
        request_id: RequestId,
        session_id: Option<SessionId>,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            request_id,
            session_id,
            outcome: ResponseOutcome::Error {
                error: ProtocolError {
                    code: code.into(),
                    message: message.into(),
                    retryable,
                },
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum EventBody {
    Hint(Hint),
    Completion(CompletionResult),
    ChatDelta { delta: String },
    ChatDone,
    ChatFailed { message: String, retryable: bool },
    InsertBuffer(InsertBufferParams),
    RequestCancelled,
    DataCleared { scope: DataClearScope },
    SessionClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub event_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub session_id: SessionId,
    #[serde(flatten)]
    pub body: EventBody,
}

impl Event {
    pub fn new(session_id: SessionId, request_id: Option<RequestId>, body: EventBody) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            request_id,
            session_id,
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Request {
        #[serde(flatten)]
        request: Request,
    },
    Response {
        #[serde(flatten)]
        response: Response,
    },
    Event {
        #[serde(flatten)]
        event: Event,
    },
}

impl From<Request> for Message {
    fn from(request: Request) -> Self {
        Self::Request { request }
    }
}

impl From<Response> for Message {
    fn from(response: Response) -> Self {
        Self::Response { response }
    }
}

impl From<Event> for Message {
    fn from(event: Event) -> Self {
        Self::Event { event }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_is_flat_and_stable() {
        let request = Request {
            request_id: RequestId(Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap()),
            session_id: None,
            body: RequestBody::Ping,
        };
        let json = serde_json::to_string(&Message::from(request.clone())).unwrap();
        assert_eq!(
            json,
            r#"{"type":"request","request_id":"00000000-0000-4000-8000-000000000001","method":"ping"}"#
        );
        let guide = include_str!("../../../docs/PROTOCOL.md");
        assert!(guide.contains(&format!("Current protocol version: **{PROTOCOL_VERSION}**")));
        assert!(guide.contains(&format!("```json\n{json}\n```")));
        assert_eq!(
            serde_json::from_str::<Message>(&json).unwrap(),
            request.into()
        );
    }

    #[test]
    fn unknown_fields_are_forward_compatible() {
        let request_id = RequestId::new();
        let json = format!(
            r#"{{"type":"request","request_id":"{request_id}","method":"ping","future":true}}"#
        );
        assert!(matches!(
            serde_json::from_str::<Message>(&json).unwrap(),
            Message::Request {
                request: Request {
                    body: RequestBody::Ping,
                    ..
                }
            }
        ));
    }

    #[test]
    fn completion_uses_documented_type_field() {
        let value = serde_json::to_value(CompletionResult {
            operation: CompletionOperation::Replace,
            command: "git pull".to_owned(),
            cursor: 8,
            description: None,
        })
        .unwrap();
        assert_eq!(value["type"], "replace");
        assert!(value.get("operation").is_none());
    }

    #[test]
    fn environment_filter_keeps_only_allowlisted_metadata() {
        let environment = BTreeMap::from([
            ("LANG".to_owned(), "zh_CN.UTF-8".to_owned()),
            ("VIRTUAL_ENV".to_owned(), "/tmp/venv\nspoof".to_owned()),
            ("OPENAI_API_KEY".to_owned(), "must-not-survive".to_owned()),
            ("AWS_SECRET_ACCESS_KEY".to_owned(), "also-secret".to_owned()),
        ]);

        let sanitized = sanitize_shell_environment(environment);
        assert_eq!(
            sanitized.get("LANG").map(String::as_str),
            Some("zh_CN.UTF-8")
        );
        assert_eq!(
            sanitized.get("VIRTUAL_ENV").map(String::as_str),
            Some("/tmp/venvspoof")
        );
        assert!(!sanitized.contains_key("OPENAI_API_KEY"));
        assert!(!sanitized.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn missing_environment_fields_deserialize_as_empty_for_old_clients() {
        let request_id = RequestId::new();
        let command_id = CommandId::new();
        let json = format!(
            r#"{{"request_id":"{request_id}","method":"command_finished","params":{{"command_id":"{command_id}","exit_code":0}}}}"#
        );
        let request: Request = serde_json::from_str(&json).unwrap();
        let RequestBody::CommandFinished(params) = request.body else {
            panic!("expected command_finished")
        };
        assert!(params.environment.is_empty());
    }

    #[test]
    fn checkpoint_request_round_trips_with_a_flat_action() {
        let request = Request::new(
            Some(SessionId::new()),
            RequestBody::Checkpoint(CheckpointParams {
                operation: CheckpointOperation::Resolve {
                    resolution: "Pinned the SDK".to_owned(),
                },
                exclude_active_command: true,
            }),
        );
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""method":"checkpoint""#));
        assert!(json.contains(r#""action":"resolve""#));
        assert!(json.contains(r#""exclude_active_command":true"#));
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);

        let legacy_json = json.replace(r#","exclude_active_command":true"#, "");
        let legacy = serde_json::from_str::<Request>(&legacy_json).unwrap();
        let RequestBody::Checkpoint(params) = legacy.body else {
            panic!("expected checkpoint request")
        };
        assert!(!params.exclude_active_command);
    }

    #[test]
    fn data_request_and_content_free_inventory_round_trip() {
        let session_id = SessionId::new();
        let request = Request::new(
            Some(session_id),
            RequestBody::Data(DataParams {
                operation: DataOperation::ClearSession,
                exclude_active_command: true,
            }),
        );
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""method":"data""#));
        assert!(json.contains(r#""action":"clear_session""#));
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        let legacy = json.replace(r#","exclude_active_command":true"#, "");
        let legacy = serde_json::from_str::<Request>(&legacy).unwrap();
        let RequestBody::Data(params) = legacy.body else {
            panic!("expected data request")
        };
        assert!(!params.exclude_active_command);

        let result = DaemonDataResult::Inventory {
            sessions: vec![SessionDataSummary {
                session_id,
                connected: true,
                command_records: 2,
                chat_messages: 3,
                environment_values: 1,
                checkpoint_present: true,
                environment_baseline_present: false,
                in_flight_commands: 0,
                discarded_finish_markers: 0,
                active_ai_requests: 0,
                pending_failure: false,
            }],
            source_card_cache_entries: 1,
            limits: SessionDataLimits {
                max_commands_per_session: 30,
                max_output_chars_per_command: 20_000,
                max_total_context_chars_per_session: 100_000,
                chat_history_enabled: true,
                max_chat_messages_per_session: 50,
                max_sessions: 64,
                disconnected_session_ttl_seconds: 3_600,
            },
        };
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(encoded.contains(r#""command_records":2"#));
        assert_eq!(
            serde_json::from_str::<DaemonDataResult>(&encoded).unwrap(),
            result
        );
    }

    #[test]
    fn old_session_context_without_checkpoint_remains_compatible() {
        let session_id = SessionId::new();
        let json = format!(
            r#"{{"session_id":"{session_id}","tty":"/dev/ttys001","cwd":"/tmp","shell":"zsh","commands":[]}}"#
        );
        let context: SessionContext = serde_json::from_str(&json).unwrap();
        assert!(context.checkpoint.is_none());
    }

    #[test]
    fn old_insert_requests_without_a_safety_label_remain_compatible() {
        let request_id = RequestId::new();
        let json = format!(
            r#"{{"request_id":"{request_id}","method":"insert_buffer","params":{{"command":"echo ok","mode":"replace"}}}}"#
        );
        let request: Request = serde_json::from_str(&json).unwrap();
        let RequestBody::InsertBuffer(params) = request.body else {
            panic!("expected insert-buffer request")
        };
        assert_eq!(params.command, "echo ok");
        assert!(params.safety.is_none());
    }
}
