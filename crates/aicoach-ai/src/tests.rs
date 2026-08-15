use std::{collections::VecDeque, sync::Arc, time::Duration};

use futures_util::StreamExt;
use secrecy::SecretString;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;

use crate::{
    AiError, AiModels, AiOperation, AiProvider, AiTimeouts, AnalysisInput, ChatMessage,
    ChatRequest, CommandCompletionRequest, NoopAiProvider, OpenAiCompatibleProvider, OpenAiConfig,
    RetryPolicy,
};

const TEST_KEY: &str = "test-placeholder-key-never-a-real-credential";

#[derive(Clone)]
struct MockReply {
    status: u16,
    content_type: &'static str,
    delay_before_headers: Duration,
    chunks: Vec<(Duration, Vec<u8>)>,
}

impl MockReply {
    fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: "application/json",
            delay_before_headers: Duration::ZERO,
            chunks: vec![(Duration::ZERO, body.into())],
        }
    }

    fn delayed_json(delay: Duration, body: impl Into<Vec<u8>>) -> Self {
        Self {
            delay_before_headers: delay,
            ..Self::json(200, body)
        }
    }

    fn sse(chunks: Vec<(Duration, impl Into<Vec<u8>>)>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            delay_before_headers: Duration::ZERO,
            chunks: chunks
                .into_iter()
                .map(|(delay, chunk)| (delay, chunk.into()))
                .collect(),
        }
    }
}

struct MockServer {
    base_url: String,
    requests: mpsc::UnboundedReceiver<String>,
}

impl MockServer {
    async fn start(replies: impl IntoIterator<Item = MockReply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let address = listener.local_addr().expect("mock server address");
        let replies = Arc::new(Mutex::new(replies.into_iter().collect::<VecDeque<_>>()));
        let (request_sender, requests) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let reply = {
                    let mut replies = replies.lock().await;
                    replies.pop_front()
                };
                let Some(reply) = reply else {
                    return;
                };
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request_sender = request_sender.clone();
                tokio::spawn(async move {
                    let request = read_http_request(&mut socket).await;
                    let _ = request_sender.send(request);
                    write_http_reply(&mut socket, reply).await;
                });
            }
        });
        Self {
            base_url: format!("http://{address}/api/v1/"),
            requests,
        }
    }

    async fn request(&mut self) -> String {
        self.requests.recv().await.expect("captured request")
    }
}

async fn read_http_request(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    let mut expected_length = None;
    loop {
        let read = socket.read(&mut buffer).await.expect("read mock request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if expected_length.is_none() {
            if let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                expected_length = Some(header_end + 4 + content_length);
            }
        }
        if expected_length.is_some_and(|length| bytes.len() >= length) {
            break;
        }
    }
    String::from_utf8(bytes).expect("request is UTF-8")
}

async fn write_http_reply(socket: &mut TcpStream, reply: MockReply) {
    tokio::time::sleep(reply.delay_before_headers).await;
    let content_length = reply
        .chunks
        .iter()
        .map(|(_, chunk)| chunk.len())
        .sum::<usize>();
    let reason = match reply.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Test Status",
    };
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status, reason, reply.content_type, content_length
    );
    if socket.write_all(headers.as_bytes()).await.is_err() {
        return;
    }
    for (delay, chunk) in reply.chunks {
        tokio::time::sleep(delay).await;
        if socket.write_all(&chunk).await.is_err() {
            return;
        }
        let _ = socket.flush().await;
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn config(base_url: String) -> OpenAiConfig {
    OpenAiConfig {
        base_url,
        api_key_env: "AICOACH_TEST_KEY".to_owned(),
        models: AiModels {
            completion: "fast-completion".to_owned(),
            analysis: "fast-analysis".to_owned(),
            chat: "smart-chat".to_owned(),
        },
        temperature: 0.2,
        timeouts: AiTimeouts {
            completion: Duration::from_millis(500),
            analysis: Duration::from_millis(500),
            chat: Duration::from_millis(500),
        },
        max_concurrency: 2,
        retry: RetryPolicy {
            max_retries: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        },
    }
}

fn provider(server: &MockServer) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new_for_test(config(server.base_url.clone()), TEST_KEY)
        .expect("test provider")
}

fn chat_request() -> ChatRequest {
    ChatRequest::new([ChatMessage::user("why did this fail?")])
}

fn envelope(content: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-test",
        "model": "returned-model",
        "choices": [{
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }
    })
    .to_string()
}

#[tokio::test]
async fn chat_uses_joined_endpoint_bearer_and_smart_model() {
    let mut server = MockServer::start([MockReply::json(200, envelope("Use `tree`."))]).await;
    let provider = provider(&server);

    assert_eq!(
        provider.endpoint().as_str(),
        format!("{}chat/completions", server.base_url)
    );
    let response = provider
        .chat(chat_request(), CancellationToken::new())
        .await
        .expect("chat response");
    assert_eq!(response.content, "Use `tree`.");
    assert_eq!(response.model.as_deref(), Some("returned-model"));
    assert_eq!(response.usage.expect("usage").total_tokens, 5);

    let request = server.request().await;
    assert!(request.starts_with("POST /api/v1/chat/completions HTTP/1.1\r\n"));
    assert!(request.contains(&format!("authorization: Bearer {TEST_KEY}")));
    let body = request.split_once("\r\n\r\n").expect("HTTP body").1;
    let body: serde_json::Value = serde_json::from_str(body).expect("JSON request");
    assert_eq!(body["model"], "smart-chat");
    assert_eq!(body["stream"], false);
}

#[tokio::test]
async fn completion_accepts_markdown_fenced_json_and_fast_model() {
    let result = r#"```json
{"type":"replace","command":"docker ps --format","cursor":18,"description":"Complete the option"}
```"#;
    let mut server = MockServer::start([MockReply::json(200, envelope(result))]).await;
    let provider = provider(&server);
    let completion = provider
        .complete_command(
            CommandCompletionRequest {
                buffer: "docker ps --forma".to_owned(),
                cursor: 17,
                cwd: "/tmp".into(),
                shell: "zsh".to_owned(),
                context: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await
        .expect("completion response");
    assert_eq!(completion.command, "docker ps --format");
    assert_eq!(completion.cursor, 18);

    let request = server.request().await;
    let body = request.split_once("\r\n\r\n").expect("HTTP body").1;
    let body: serde_json::Value = serde_json::from_str(body).expect("JSON request");
    assert_eq!(body["model"], "fast-completion");
    assert_eq!(body["response_format"]["type"], "json_object");
}

#[tokio::test]
async fn analysis_parses_typed_json_and_uses_analysis_model() {
    let analysis = serde_json::json!({
        "needResponse": true,
        "severity": "error",
        "category": "command_not_found",
        "title": "Command not found",
        "message": "The command name appears to be misspelled.",
        "suggestedCommand": "tree",
        "confidence": 0.97
    })
    .to_string();
    let mut server = MockServer::start([MockReply::json(200, envelope(&analysis))]).await;
    let result = provider(&server)
        .analyze_command(
            AnalysisInput::new("treee", 127, "/tmp"),
            CancellationToken::new(),
        )
        .await
        .expect("analysis response");
    assert!(result.need_response);
    assert_eq!(result.suggested_command.as_deref(), Some("tree"));
    assert!((result.confidence - 0.97).abs() < f32::EPSILON);

    let request = server.request().await;
    let body = request.split_once("\r\n\r\n").expect("HTTP body").1;
    let body: serde_json::Value = serde_json::from_str(body).expect("JSON request");
    assert_eq!(body["model"], "fast-analysis");
    assert_eq!(body["response_format"]["type"], "json_object");
}

#[tokio::test]
async fn unicode_completion_cursor_uses_zle_character_offsets() {
    let result =
        r#"{"type":"replace","command":"echo 你好","cursor":7,"description":"Unicode command"}"#;
    let server = MockServer::start([MockReply::json(200, envelope(result))]).await;
    let completion = provider(&server)
        .complete_command(
            CommandCompletionRequest::new("echo 你", 6, "/tmp", "zsh"),
            CancellationToken::new(),
        )
        .await
        .expect("character cursor must be accepted");
    assert_eq!(completion.cursor, 7);
    assert_eq!(completion.command.len(), 11, "UTF-8 byte length differs");
}

#[tokio::test]
async fn structured_response_rejects_invalid_json() {
    let server = MockServer::start([MockReply::json(200, envelope("not JSON"))]).await;
    let error = provider(&server)
        .complete_command(
            CommandCompletionRequest {
                buffer: "git sta".to_owned(),
                cursor: 7,
                cwd: "/tmp".into(),
                shell: "zsh".to_owned(),
                context: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid JSON must fail");
    assert!(matches!(
        error,
        AiError::InvalidResponse {
            operation: AiOperation::Completion,
            reason: "message content does not contain valid JSON"
        }
    ));
}

#[tokio::test]
async fn structured_response_rejects_missing_fields() {
    let server = MockServer::start([MockReply::json(
        200,
        envelope(r#"{"type":"replace","command":"git status"}"#),
    )])
    .await;
    let error = provider(&server)
        .complete_command(
            CommandCompletionRequest {
                buffer: "git sta".to_owned(),
                cursor: 7,
                cwd: "/tmp".into(),
                shell: "zsh".to_owned(),
                context: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("missing fields must fail");
    assert!(matches!(
        error,
        AiError::InvalidResponse {
            reason: "structured response is missing required fields",
            ..
        }
    ));
}

#[tokio::test]
async fn completion_has_its_own_timeout() {
    let server = MockServer::start([MockReply::delayed_json(
        Duration::from_millis(150),
        envelope("ignored"),
    )])
    .await;
    let mut test_config = config(server.base_url.clone());
    test_config.timeouts.completion = Duration::from_millis(25);
    test_config.timeouts.chat = Duration::from_secs(1);
    let provider = OpenAiCompatibleProvider::new_for_test(test_config, TEST_KEY).expect("provider");
    let error = provider
        .complete_command(
            CommandCompletionRequest {
                buffer: "git sta".to_owned(),
                cursor: 7,
                cwd: "/tmp".into(),
                shell: "zsh".to_owned(),
                context: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("request should time out");
    assert!(matches!(
        error,
        AiError::Timeout {
            operation: AiOperation::Completion
        }
    ));
}

#[tokio::test]
async fn chat_and_analysis_use_their_respective_timeouts() {
    let chat_server = MockServer::start([MockReply::delayed_json(
        Duration::from_millis(100),
        envelope("late"),
    )])
    .await;
    let mut chat_config = config(chat_server.base_url.clone());
    chat_config.timeouts.chat = Duration::from_millis(20);
    chat_config.timeouts.analysis = Duration::from_secs(1);
    let chat_error = OpenAiCompatibleProvider::new_for_test(chat_config, TEST_KEY)
        .expect("provider")
        .chat(chat_request(), CancellationToken::new())
        .await
        .expect_err("chat should use the chat timeout");
    assert!(matches!(
        chat_error,
        AiError::Timeout {
            operation: AiOperation::Chat
        }
    ));

    let analysis_server = MockServer::start([MockReply::delayed_json(
        Duration::from_millis(100),
        envelope("late"),
    )])
    .await;
    let mut analysis_config = config(analysis_server.base_url.clone());
    analysis_config.timeouts.analysis = Duration::from_millis(20);
    analysis_config.timeouts.chat = Duration::from_secs(1);
    let analysis_error = OpenAiCompatibleProvider::new_for_test(analysis_config, TEST_KEY)
        .expect("provider")
        .analyze_command(
            AnalysisInput::new("false", 1, "/tmp"),
            CancellationToken::new(),
        )
        .await
        .expect_err("analysis should use the analysis timeout");
    assert!(matches!(
        analysis_error,
        AiError::Timeout {
            operation: AiOperation::Analysis
        }
    ));
}

#[tokio::test]
async fn request_can_be_cancelled_while_waiting_for_headers() {
    let server = MockServer::start([MockReply::delayed_json(
        Duration::from_secs(1),
        envelope("ignored"),
    )])
    .await;
    let provider = provider(&server);
    let cancellation = CancellationToken::new();
    let cancel_from_test = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel_from_test.cancel();
    });
    let error = provider
        .chat(chat_request(), cancellation)
        .await
        .expect_err("request should be cancelled");
    assert!(matches!(
        error,
        AiError::Cancelled {
            operation: AiOperation::Chat
        }
    ));
}

#[tokio::test]
async fn stream_decodes_split_sse_chunks_and_done_marker() {
    let server = MockServer::start([MockReply::sse(vec![
        (
            Duration::ZERO,
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel",
        ),
        (
            Duration::from_millis(5),
            "lo\"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
        ),
        (Duration::ZERO, "data: [DONE]\n\n"),
    ])])
    .await;
    let mut stream = provider(&server)
        .stream_chat(chat_request(), CancellationToken::new())
        .await
        .expect("open stream");
    let mut text = String::new();
    while let Some(delta) = stream.next().await {
        text.push_str(&delta.expect("valid stream delta"));
    }
    assert_eq!(text, "hello world");
}

#[tokio::test]
async fn malformed_stream_chunk_is_reported_without_response_body_leak() {
    let server = MockServer::start([MockReply::sse(vec![(
        Duration::ZERO,
        "data: this-is-not-json\n\n",
    )])])
    .await;
    let mut stream = provider(&server)
        .stream_chat(chat_request(), CancellationToken::new())
        .await
        .expect("open stream");
    let error = stream
        .next()
        .await
        .expect("protocol error item")
        .expect_err("malformed chunk must fail");
    assert!(matches!(error, AiError::StreamProtocol { .. }));
    assert!(!error.to_string().contains("this-is-not-json"));
}

#[tokio::test]
async fn stream_timeout_applies_after_headers() {
    let server = MockServer::start([MockReply::sse(vec![
        (
            Duration::ZERO,
            "data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n",
        ),
        (Duration::from_secs(1), "data: [DONE]\n\n"),
    ])])
    .await;
    let mut test_config = config(server.base_url.clone());
    test_config.timeouts.chat = Duration::from_millis(40);
    let mut stream = OpenAiCompatibleProvider::new_for_test(test_config, TEST_KEY)
        .expect("provider")
        .stream_chat(chat_request(), CancellationToken::new())
        .await
        .expect("open stream");
    assert_eq!(
        stream.next().await.expect("first item").expect("delta"),
        "first"
    );
    assert!(matches!(
        stream.next().await.expect("timeout item"),
        Err(AiError::Timeout {
            operation: AiOperation::Chat
        })
    ));
}

#[tokio::test]
async fn streaming_request_can_be_cancelled_mid_response() {
    let server = MockServer::start([MockReply::sse(vec![
        (
            Duration::ZERO,
            "data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n",
        ),
        (
            Duration::from_secs(1),
            "data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n",
        ),
    ])])
    .await;
    let cancellation = CancellationToken::new();
    let mut stream = provider(&server)
        .stream_chat(chat_request(), cancellation.clone())
        .await
        .expect("open stream");
    assert_eq!(
        stream.next().await.expect("first item").expect("delta"),
        "first"
    );
    cancellation.cancel();
    assert!(matches!(
        stream.next().await.expect("cancellation item"),
        Err(AiError::Cancelled {
            operation: AiOperation::Chat
        })
    ));
}

#[tokio::test]
async fn concurrency_permit_is_held_for_the_stream_lifetime() {
    let mut server = MockServer::start([
        MockReply::sse(vec![
            (
                Duration::ZERO,
                "data: {\"choices\":[{\"delta\":{\"content\":\"held\"}}]}\n\n",
            ),
            (Duration::from_secs(1), "data: [DONE]\n\n"),
        ]),
        MockReply::json(200, envelope("second")),
    ])
    .await;
    let mut test_config = config(server.base_url.clone());
    test_config.max_concurrency = 1;
    test_config.timeouts.chat = Duration::from_secs(2);
    let provider = OpenAiCompatibleProvider::new_for_test(test_config, TEST_KEY).expect("provider");

    let mut stream = provider
        .stream_chat(chat_request(), CancellationToken::new())
        .await
        .expect("open stream");
    assert_eq!(
        stream.next().await.expect("first item").expect("delta"),
        "held"
    );
    let _first_request = server.request().await;

    let second_provider = provider.clone();
    let second = tokio::spawn(async move {
        second_provider
            .chat(chat_request(), CancellationToken::new())
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), server.requests.recv())
            .await
            .is_err(),
        "second request must wait for the stream permit"
    );

    drop(stream);
    assert_eq!(
        second
            .await
            .expect("chat task")
            .expect("second response")
            .content,
        "second"
    );
    let _second_request = server.request().await;
}

#[tokio::test]
async fn transient_server_error_is_retried_but_client_error_is_not() {
    let mut retry_server = MockServer::start([
        MockReply::json(503, "temporary"),
        MockReply::json(200, envelope("recovered")),
    ])
    .await;
    let response = provider(&retry_server)
        .chat(chat_request(), CancellationToken::new())
        .await
        .expect("retry succeeds");
    assert_eq!(response.content, "recovered");
    let _ = retry_server.request().await;
    let _ = retry_server.request().await;

    let mut rate_limit_server = MockServer::start([
        MockReply::json(429, "rate limited"),
        MockReply::json(200, envelope("recovered after rate limit")),
    ])
    .await;
    let response = provider(&rate_limit_server)
        .chat(chat_request(), CancellationToken::new())
        .await
        .expect("rate limit retry succeeds");
    assert_eq!(response.content, "recovered after rate limit");
    let _ = rate_limit_server.request().await;
    let _ = rate_limit_server.request().await;

    let mut client_error_server = MockServer::start([
        MockReply::json(400, "bad request"),
        MockReply::json(200, envelope("must not be requested")),
    ])
    .await;
    let error = provider(&client_error_server)
        .chat(chat_request(), CancellationToken::new())
        .await
        .expect_err("4xx fails immediately");
    assert!(matches!(error, AiError::HttpStatus { status: 400, .. }));
    let _ = client_error_server.request().await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            client_error_server.requests.recv()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn noop_provider_is_explicitly_offline() {
    assert!(matches!(
        NoopAiProvider
            .chat(chat_request(), CancellationToken::new())
            .await,
        Err(AiError::Offline)
    ));
}

#[test]
fn credential_is_redacted_from_all_error_text() {
    let bad = OpenAiCompatibleProvider::new_for_test(
        OpenAiConfig {
            base_url: "not a URL".to_owned(),
            ..OpenAiConfig::default()
        },
        TEST_KEY,
    )
    .err()
    .expect("bad URL");
    assert!(!bad.to_string().contains(TEST_KEY));
    assert!(!format!("{bad:?}").contains(TEST_KEY));
    assert!(!format!("{:?}", SecretString::from(TEST_KEY.to_owned())).contains(TEST_KEY));
}

#[test]
fn core_config_converts_models_timeouts_and_concurrency() {
    let core = aicoach_core::AiConfig {
        base_url: "https://example.invalid/api/v1".to_owned(),
        models: aicoach_core::AiModels {
            completion: "quick".to_owned(),
            error_analysis: "careful".to_owned(),
            chat: "conversation".to_owned(),
        },
        timeouts_ms: aicoach_core::AiTimeouts {
            completion: 321,
            error_analysis: 654,
            chat: 987,
        },
        max_concurrent_requests: 7,
        ..aicoach_core::AiConfig::default()
    };

    let converted = OpenAiConfig::from(&core);
    assert_eq!(converted.models.completion, "quick");
    assert_eq!(converted.models.analysis, "careful");
    assert_eq!(converted.models.chat, "conversation");
    assert_eq!(converted.timeouts.completion, Duration::from_millis(321));
    assert_eq!(converted.timeouts.analysis, Duration::from_millis(654));
    assert_eq!(converted.timeouts.chat, Duration::from_millis(987));
    assert_eq!(converted.max_concurrency, 7);
}

#[test]
fn base_url_already_at_completions_is_not_duplicated() {
    let test_config = OpenAiConfig {
        base_url: "https://example.invalid/api/v1/chat/completions/".to_owned(),
        ..OpenAiConfig::default()
    };
    let provider = OpenAiCompatibleProvider::new_for_test(test_config, TEST_KEY)
        .expect("full endpoint is valid");
    assert_eq!(
        provider.endpoint().as_str(),
        "https://example.invalid/api/v1/chat/completions"
    );
}
