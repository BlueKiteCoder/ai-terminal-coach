//! Compatibility protocol for a persistent Zsh `zsocket` connection.
//!
//! Client frames are tab-separated and start with `ZSH`. Server frames start
//! with their action (`HINT`, `COMPLETE`, `ANSWER`, `INSERT`, ...). Fields keep
//! normal UTF-8 intact while percent-escaping `%`, tab, CR, LF, and NUL, so
//! user-visible text stays readable and delimiters can never create frames.

use std::{collections::BTreeMap, path::PathBuf, str::FromStr};

use crate::protocol::{
    CancelParams, ChatParams, CommandFinishedParams, CommandId, CommandStartedParams,
    CompletionOperation, CompletionParams, ContextParams, EventBody, FocusParams, InsertMode,
    Message, RegisterSessionParams, Request, RequestBody, RequestId, ResponseOutcome,
    ResponseResult, RiskLensParams, SessionId, ShutdownParams, sanitize_shell_environment,
};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ZshProtocolError {
    #[error("empty Zsh protocol frame")]
    Empty,
    #[error("expected ZSH protocol prefix")]
    InvalidPrefix,
    #[error("unknown Zsh protocol verb: {0}")]
    UnknownVerb(String),
    #[error("{verb} expects {expected} fields, received {actual}")]
    FieldCount {
        verb: String,
        expected: usize,
        actual: usize,
    },
    #[error("invalid percent encoding")]
    InvalidPercentEncoding,
    #[error("percent-decoded field is not UTF-8")]
    InvalidUtf8,
    #[error("invalid {field}: {value}")]
    InvalidField { field: &'static str, value: String },
    #[error("message has no Zsh wire representation")]
    UnsupportedMessage,
}

pub fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => output.push_str("%25"),
            '\t' => output.push_str("%09"),
            '\n' => output.push_str("%0A"),
            '\r' => output.push_str("%0D"),
            '\0' => output.push_str("%00"),
            // Keep ordinary UTF-8 directly readable. The Zsh peer only needs
            // to decode frame delimiters and percent itself.
            other => output.push(other),
        }
    }
    output
}

pub fn percent_decode(value: &str) -> Result<String, ZshProtocolError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            if matches!(bytes[index], b'\t' | b'\n' | b'\r' | 0) {
                return Err(ZshProtocolError::InvalidPercentEncoding);
            }
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(ZshProtocolError::InvalidPercentEncoding);
        }
        let high = hex(bytes[index + 1]).ok_or(ZshProtocolError::InvalidPercentEncoding)?;
        let low = hex(bytes[index + 2]).ok_or(ZshProtocolError::InvalidPercentEncoding)?;
        output.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(output).map_err(|_| ZshProtocolError::InvalidUtf8)
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn decode_request(line: &str) -> Result<Request, ZshProtocolError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        return Err(ZshProtocolError::Empty);
    }
    let raw: Vec<&str> = line.split('\t').collect();
    if raw.first() != Some(&"ZSH") {
        return Err(ZshProtocolError::InvalidPrefix);
    }
    let verb = raw
        .get(1)
        .ok_or(ZshProtocolError::Empty)?
        .to_ascii_uppercase();
    let fields = raw[2..]
        .iter()
        .map(|field| percent_decode(field))
        .collect::<Result<Vec<_>, _>>()?;
    decode_fields(&verb, &fields)
}

fn decode_fields(verb: &str, fields: &[String]) -> Result<Request, ZshProtocolError> {
    match verb {
        "REGISTER" => {
            if !(4..=6).contains(&fields.len()) {
                return Err(ZshProtocolError::FieldCount {
                    verb: verb.to_owned(),
                    expected: 4,
                    actual: fields.len(),
                });
            }
            let requested_session_id = if fields[0].is_empty() {
                None
            } else {
                Some(parse_id::<SessionId>("session", &fields[0])?)
            };
            let pid = parse_number("pid", &fields[2])?;
            Ok(Request::new(
                None,
                RequestBody::RegisterSession(RegisterSessionParams {
                    requested_session_id,
                    tty: fields[1].clone(),
                    pid: Some(pid),
                    cwd: PathBuf::from(&fields[3]),
                    shell: "zsh".to_owned(),
                    terminal: fields.get(4).and_then(|value| nonempty(value)),
                    environment: fields
                        .get(5)
                        .map_or_else(BTreeMap::new, |value| decode_environment(value)),
                }),
            ))
        }
        "PREEXEC" => {
            expect_fields(verb, fields, 5)?;
            let session_id = parse_id("session", &fields[0])?;
            let request_id: RequestId = parse_id("request", &fields[1])?;
            Ok(Request {
                request_id,
                session_id: Some(session_id),
                body: RequestBody::CommandStarted(CommandStartedParams {
                    command_id: CommandId(request_id.0),
                    cwd: PathBuf::from(&fields[2]),
                    command: fields[3].clone(),
                    started_at_unix_ms: Some(parse_number("timestamp_ms", &fields[4])?),
                }),
            })
        }
        "FINISH" => {
            if !(fields.len() == 5 || fields.len() == 6) {
                return Err(ZshProtocolError::FieldCount {
                    verb: verb.to_owned(),
                    expected: 5,
                    actual: fields.len(),
                });
            }
            let session_id = parse_id("session", &fields[0])?;
            let request_id: RequestId = parse_id("request", &fields[1])?;
            Ok(Request {
                request_id,
                session_id: Some(session_id),
                body: RequestBody::CommandFinished(CommandFinishedParams {
                    command_id: CommandId(request_id.0),
                    command: None,
                    cwd: Some(PathBuf::from(&fields[3])),
                    exit_code: parse_number("exit", &fields[2])?,
                    stdout: None,
                    stderr: None,
                    duration_ms: Some(parse_number("duration_ms", &fields[4])?),
                    environment: fields
                        .get(5)
                        .map_or_else(BTreeMap::new, |value| decode_environment(value)),
                }),
            })
        }
        "COMPLETE" => {
            expect_fields(verb, fields, 5)?;
            Ok(Request {
                session_id: Some(parse_id("session", &fields[0])?),
                request_id: parse_id("request", &fields[1])?,
                body: RequestBody::Completion(CompletionParams {
                    cursor: parse_number("cursor", &fields[2])?,
                    cwd: PathBuf::from(&fields[3]),
                    buffer: fields[4].clone(),
                }),
            })
        }
        "LENS" => {
            expect_fields(verb, fields, 4)?;
            Ok(Request {
                session_id: Some(parse_id("session", &fields[0])?),
                request_id: parse_id("request", &fields[1])?,
                body: RequestBody::RiskLens(RiskLensParams {
                    cwd: PathBuf::from(&fields[2]),
                    buffer: fields[3].clone(),
                }),
            })
        }
        "CHAT" => {
            expect_fields(verb, fields, 5)?;
            Ok(Request {
                session_id: Some(parse_id("session", &fields[0])?),
                request_id: parse_id("request", &fields[1])?,
                body: RequestBody::Chat(ChatParams {
                    cwd: Some(PathBuf::from(&fields[2])),
                    buffer: nonempty(&fields[3]),
                    message: fields[4].clone(),
                    stream: true,
                }),
            })
        }
        "CANCEL" => {
            expect_fields(verb, fields, 2)?;
            Ok(Request::new(
                Some(parse_id("session", &fields[0])?),
                RequestBody::Cancel(CancelParams {
                    target_request_id: parse_id("request", &fields[1])?,
                }),
            ))
        }
        "FOCUS" => {
            expect_fields(verb, fields, 2)?;
            Ok(Request::new(
                Some(parse_id("session", &fields[0])?),
                RequestBody::Focus(FocusParams {
                    tty: fields[1].clone(),
                }),
            ))
        }
        "CONTEXT" => {
            if !(fields.len() == 1 || fields.len() == 3) {
                return Err(ZshProtocolError::FieldCount {
                    verb: verb.to_owned(),
                    expected: 1,
                    actual: fields.len(),
                });
            }
            let request_id = fields
                .get(1)
                .map(|value| parse_id("request", value))
                .transpose()?
                .unwrap_or_default();
            let max_commands = fields
                .get(2)
                .filter(|value| !value.is_empty())
                .map(|value| parse_number("max_commands", value))
                .transpose()?;
            Ok(Request {
                session_id: Some(parse_id("session", &fields[0])?),
                request_id,
                body: RequestBody::Context(ContextParams { max_commands }),
            })
        }
        "INSERT" => {
            expect_fields(verb, fields, 3)?;
            Ok(Request::new(
                Some(parse_id("session", &fields[0])?),
                RequestBody::InsertBuffer(crate::protocol::InsertBufferParams {
                    command: fields[2].clone(),
                    cursor: None,
                    mode: InsertMode::Replace,
                }),
            ))
        }
        "DISCONNECT" => {
            expect_fields(verb, fields, 1)?;
            Ok(Request::new(
                Some(parse_id("session", &fields[0])?),
                RequestBody::Disconnect,
            ))
        }
        "PING" => {
            if fields.len() > 2 {
                return Err(ZshProtocolError::FieldCount {
                    verb: verb.to_owned(),
                    expected: 2,
                    actual: fields.len(),
                });
            }
            Ok(Request {
                session_id: fields
                    .first()
                    .filter(|value| !value.is_empty())
                    .map(|value| parse_id("session", value))
                    .transpose()?,
                request_id: fields
                    .get(1)
                    .filter(|value| !value.is_empty())
                    .map(|value| parse_id("request", value))
                    .transpose()?
                    .unwrap_or_default(),
                body: RequestBody::Ping,
            })
        }
        "SHUTDOWN" => Ok(Request::new(
            fields
                .first()
                .filter(|value| !value.is_empty())
                .map(|value| parse_id("session", value))
                .transpose()?,
            RequestBody::Shutdown(ShutdownParams {
                reason: fields.get(1).and_then(|value| nonempty(value)),
            }),
        )),
        other => Err(ZshProtocolError::UnknownVerb(other.to_owned())),
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn decode_environment(value: &str) -> BTreeMap<String, String> {
    let environment = value
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    sanitize_shell_environment(environment)
}

fn expect_fields(verb: &str, fields: &[String], expected: usize) -> Result<(), ZshProtocolError> {
    if fields.len() == expected {
        Ok(())
    } else {
        Err(ZshProtocolError::FieldCount {
            verb: verb.to_owned(),
            expected,
            actual: fields.len(),
        })
    }
}

fn parse_id<T>(field: &'static str, value: &str) -> Result<T, ZshProtocolError>
where
    T: FromStr<Err = uuid::Error>,
{
    value.parse().map_err(|_| ZshProtocolError::InvalidField {
        field,
        value: value.to_owned(),
    })
}

fn parse_number<T>(field: &'static str, value: &str) -> Result<T, ZshProtocolError>
where
    T: FromStr,
{
    value.parse().map_err(|_| ZshProtocolError::InvalidField {
        field,
        value: value.to_owned(),
    })
}

pub fn encode_message(message: &Message) -> Result<String, ZshProtocolError> {
    let fields: Vec<String> = match message {
        Message::Response { response } => match &response.outcome {
            ResponseOutcome::Error { error } => vec![
                "ERROR".to_owned(),
                response
                    .session_id
                    .map_or_else(String::new, |value| value.to_string()),
                response.request_id.to_string(),
                error.code.clone(),
                error.message.clone(),
                error.retryable.to_string(),
            ],
            ResponseOutcome::Ok { result } => match result {
                ResponseResult::Hello {
                    protocol_version,
                    server_version,
                } => vec![
                    "WELCOME".to_owned(),
                    response.request_id.to_string(),
                    protocol_version.to_string(),
                    server_version.clone(),
                ],
                ResponseResult::SessionRegistered { session_id } => vec![
                    "REGISTERED".to_owned(),
                    session_id.to_string(),
                    response.request_id.to_string(),
                ],
                ResponseResult::Accepted => vec![
                    "OK".to_owned(),
                    response
                        .session_id
                        .map_or_else(String::new, |value| value.to_string()),
                    response.request_id.to_string(),
                ],
                ResponseResult::Completion(completion) => {
                    completion_fields(response.session_id, Some(response.request_id), completion)
                }
                ResponseResult::RiskLens(result) => vec![
                    "LENS".to_owned(),
                    response
                        .session_id
                        .map_or_else(String::new, |value| value.to_string()),
                    response.request_id.to_string(),
                    result.report.level.map_or_else(
                        || "unrated".to_owned(),
                        |level| level.to_string().to_ascii_lowercase(),
                    ),
                    result.message.clone(),
                ],
                ResponseResult::Chat { message } => vec![
                    "ANSWER".to_owned(),
                    response
                        .session_id
                        .map_or_else(String::new, |value| value.to_string()),
                    response.request_id.to_string(),
                    message.clone(),
                ],
                ResponseResult::Context(context) => vec![
                    "CONTEXT".to_owned(),
                    context.session_id.to_string(),
                    response.request_id.to_string(),
                    serde_json::to_string(context)
                        .map_err(|_| ZshProtocolError::UnsupportedMessage)?,
                ],
                ResponseResult::Pong { .. } => vec!["PONG".to_owned()],
                ResponseResult::ShutdownAccepted => {
                    vec!["SHUTDOWN".to_owned(), response.request_id.to_string()]
                }
            },
        },
        Message::Event { event } => match &event.body {
            EventBody::Hint(hint) => vec![
                "HINT".to_owned(),
                event.session_id.to_string(),
                event
                    .request_id
                    .map_or_else(String::new, |value| value.to_string()),
                format!("{:?}", hint.severity).to_ascii_lowercase(),
                hint.message.clone(),
                hint.suggested_command.clone().unwrap_or_default(),
            ],
            EventBody::Completion(completion) => {
                completion_fields(Some(event.session_id), event.request_id, completion)
            }
            EventBody::ChatDelta { delta } => vec![
                "ANSWER_DELTA".to_owned(),
                event.session_id.to_string(),
                event
                    .request_id
                    .map_or_else(String::new, |value| value.to_string()),
                delta.clone(),
            ],
            EventBody::ChatDone => vec![
                "ANSWER_DONE".to_owned(),
                event.session_id.to_string(),
                event
                    .request_id
                    .map_or_else(String::new, |value| value.to_string()),
            ],
            EventBody::ChatFailed { message, retryable } => vec![
                "ERROR".to_owned(),
                event.session_id.to_string(),
                event
                    .request_id
                    .map_or_else(String::new, |value| value.to_string()),
                "ai_unavailable".to_owned(),
                message.clone(),
                retryable.to_string(),
            ],
            EventBody::InsertBuffer(insert) => vec![
                "INSERT".to_owned(),
                event.session_id.to_string(),
                insert.command.clone(),
            ],
            EventBody::RequestCancelled => vec![
                "CANCELLED".to_owned(),
                event.session_id.to_string(),
                event
                    .request_id
                    .map_or_else(String::new, |value| value.to_string()),
            ],
            EventBody::SessionClosed => vec!["CLOSED".to_owned(), event.session_id.to_string()],
        },
        Message::Request { .. } => return Err(ZshProtocolError::UnsupportedMessage),
    };
    Ok(fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            if index == 0 {
                field.clone()
            } else {
                percent_encode(field)
            }
        })
        .collect::<Vec<_>>()
        .join("\t"))
}

fn completion_fields(
    session_id: Option<SessionId>,
    request_id: Option<RequestId>,
    completion: &crate::protocol::CompletionResult,
) -> Vec<String> {
    vec![
        "COMPLETE".to_owned(),
        session_id.map_or_else(String::new, |value| value.to_string()),
        request_id.map_or_else(String::new, |value| value.to_string()),
        match completion.operation {
            CompletionOperation::Replace => "replace",
            CompletionOperation::Insert => "insert",
            CompletionOperation::Suggest => "suggest",
        }
        .to_owned(),
        completion.cursor.to_string(),
        completion.command.clone(),
        completion.description.clone().unwrap_or_default(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicoach_core::{
        AnalysisCoverage, EffectAction, PrivilegeRequirement, RecoveryProspect, RiskEffect,
        RiskLensReport, RiskLevel,
    };

    use crate::protocol::{CompletionResult, Event, RiskLensResult, Severity};

    #[test]
    fn percent_encoding_round_trip_covers_frame_delimiters_and_unicode() {
        let source = "命令\tline\n100%\r";
        let encoded = percent_encode(source);
        assert!(encoded.contains("%09"));
        assert!(encoded.contains("%0A"));
        assert!(encoded.contains("%0D"));
        assert!(encoded.contains("%25"));
        assert!(encoded.contains("命令"));
        assert_eq!(percent_decode(&encoded).unwrap(), source);
    }

    #[test]
    fn decodes_completion_request() {
        let session = SessionId::new();
        let request = RequestId::new();
        let line = format!("ZSH\tCOMPLETE\t{session}\t{request}\t17\t/tmp/demo\tdocker ps --forma");
        let decoded = decode_request(&line).unwrap();
        assert_eq!(decoded.session_id, Some(session));
        assert_eq!(decoded.request_id, request);
        assert!(matches!(decoded.body, RequestBody::Completion(_)));
    }

    #[test]
    fn decodes_risk_lens_request() {
        let session = SessionId::new();
        let request = RequestId::new();
        let line = format!("ZSH\tLENS\t{session}\t{request}\t/tmp/demo\tgit reset --hard");
        let decoded = decode_request(&line).unwrap();
        let RequestBody::RiskLens(params) = decoded.body else {
            panic!("expected risk lens request")
        };
        assert_eq!(decoded.session_id, Some(session));
        assert_eq!(decoded.request_id, request);
        assert_eq!(params.buffer, "git reset --hard");
        assert_eq!(params.cwd, PathBuf::from("/tmp/demo"));
    }

    #[test]
    fn shell_chat_requests_streaming_responses() {
        let session = SessionId::new();
        let request = RequestId::new();
        let line = format!("ZSH\tCHAT\t{session}\t{request}\t/tmp\t\t为什么失败");
        let decoded = decode_request(&line).unwrap();
        let RequestBody::Chat(params) = decoded.body else {
            panic!("expected chat request")
        };
        assert!(params.stream);
        assert_eq!(params.message, "为什么失败");
    }

    #[test]
    fn encodes_exact_shell_completion_shape() {
        let session = SessionId::new();
        let request = RequestId::new();
        let event = Event::new(
            session,
            Some(request),
            EventBody::Completion(CompletionResult {
                operation: CompletionOperation::Replace,
                command: "docker ps --format\t{{.ID}}".to_owned(),
                cursor: 26,
                description: Some("修正补全".to_owned()),
            }),
        );
        let encoded = encode_message(&Message::from(event)).unwrap();
        assert!(encoded.starts_with(&format!("COMPLETE\t{session}\t{request}\treplace\t26\t")));
        assert_eq!(encoded.matches('\t').count(), 6);
        assert!(encoded.contains("%09"));
    }

    #[test]
    fn encodes_multiline_risk_lens_result_for_zsh() {
        let session = SessionId::new();
        let request = Request::new(
            Some(session),
            RequestBody::RiskLens(RiskLensParams {
                buffer: "git reset --hard".to_owned(),
                cwd: PathBuf::from("/tmp/demo"),
            }),
        );
        let request_id = request.request_id;
        let response = crate::protocol::Response::ok(
            &request,
            ResponseResult::RiskLens(RiskLensResult {
                report: RiskLensReport {
                    level: Some(RiskLevel::High),
                    effects: vec![RiskEffect {
                        action: EffectAction::Modify,
                        target: "Git worktree".to_owned(),
                    }],
                    privilege: PrivilegeRequirement::CurrentUser,
                    recovery: RecoveryProspect::Limited,
                    coverage: AnalysisCoverage::Recognized,
                    safety_rules_enabled: true,
                    rule_ids: vec!["git.reset-hard".to_owned()],
                },
                source_cards: Vec::new(),
                message: "Risk Lens · HIGH\nImpact: modify Git worktree".to_owned(),
            }),
        );
        let encoded = encode_message(&Message::from(response)).unwrap();
        assert!(encoded.starts_with(&format!("LENS\t{session}\t{request_id}\thigh\t")));
        assert!(encoded.contains("%0AImpact"));
    }

    #[test]
    fn encodes_hint_shape() {
        let session = SessionId::new();
        let event = Event::new(
            session,
            None,
            EventBody::Hint(crate::protocol::Hint {
                severity: Severity::Critical,
                title: "危险".to_owned(),
                message: "不要执行".to_owned(),
                suggested_command: None,
            }),
        );
        let encoded = encode_message(&Message::from(event)).unwrap();
        assert!(encoded.starts_with(&format!("HINT\t{session}\t\tcritical\t")));
    }

    #[test]
    fn encodes_stream_failure_as_shell_error() {
        let session = SessionId::new();
        let request = RequestId::new();
        let encoded = encode_message(&Message::from(Event::new(
            session,
            Some(request),
            EventBody::ChatFailed {
                message: "interrupted".to_owned(),
                retryable: true,
            },
        )))
        .unwrap();
        assert!(encoded.starts_with(&format!(
            "ERROR\t{session}\t{request}\tai_unavailable\tinterrupted\ttrue"
        )));
    }

    #[test]
    fn register_accepts_allowlisted_environment_extension() {
        let session = SessionId::new();
        let environment =
            percent_encode("LANG=zh_CN.UTF-8\nTERM=xterm-256color\nOPENAI_API_KEY=not-allowed\n");
        let line = format!(
            "ZSH\tREGISTER\t{session}\t/dev/ttys001\t42\t/tmp\tApple_Terminal\t{environment}"
        );
        let request = decode_request(&line).unwrap();
        let RequestBody::RegisterSession(params) = request.body else {
            panic!("expected register request")
        };
        assert_eq!(params.environment.len(), 2);
        assert_eq!(
            params.environment.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
        assert!(!params.environment.contains_key("OPENAI_API_KEY"));
    }

    #[test]
    fn finish_accepts_environment_extension_and_legacy_shape() {
        let session = SessionId::new();
        let request = RequestId::new();
        let legacy = format!("ZSH\tFINISH\t{session}\t{request}\t0\t/tmp\t25");
        let decoded = decode_request(&legacy).unwrap();
        let RequestBody::CommandFinished(params) = decoded.body else {
            panic!("expected command_finished")
        };
        assert!(params.environment.is_empty());

        let environment = percent_encode("CONDA_DEFAULT_ENV=dev=tools\nLC_ALL=C.UTF-8\n");
        let extended = format!("ZSH\tFINISH\t{session}\t{request}\t1\t/tmp\t30\t{environment}");
        let decoded = decode_request(&extended).unwrap();
        let RequestBody::CommandFinished(params) = decoded.body else {
            panic!("expected command_finished")
        };
        assert_eq!(
            params
                .environment
                .get("CONDA_DEFAULT_ENV")
                .map(String::as_str),
            Some("dev=tools")
        );
        assert_eq!(
            params.environment.get("LC_ALL").map(String::as_str),
            Some("C.UTF-8")
        );
    }
}
