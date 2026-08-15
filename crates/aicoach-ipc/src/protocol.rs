use std::{collections::BTreeMap, fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertMode {
    Replace,
    Insert,
    Suggest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsertBufferParams {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<usize>,
    #[serde(default = "replace_mode")]
    pub mode: InsertMode,
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
    Cancel(CancelParams),
    Chat(ChatParams),
    Context(ContextParams),
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
pub struct SessionContext {
    pub session_id: SessionId,
    pub tty: String,
    pub cwd: PathBuf,
    pub shell: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    pub commands: Vec<ContextCommand>,
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
    Chat {
        message: String,
    },
    Context(SessionContext),
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
        let request = Request::new(None, RequestBody::Ping);
        let json = serde_json::to_string(&Message::from(request.clone())).unwrap();
        assert!(json.contains(r#""type":"request""#));
        assert!(json.contains(r#""method":"ping""#));
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
}
