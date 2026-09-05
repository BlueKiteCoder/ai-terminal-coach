#![allow(
    clippy::assigning_clones,
    clippy::cast_possible_truncation,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::semicolon_if_nothing_returned,
    clippy::struct_excessive_bools
)]

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use aicoach_core::{
    AnalysisCoverage, Config, ProductPaths, RiskLevel, SafetyEngine, strip_terminal_sequences,
};
use aicoach_ipc::{
    ChatParams, ClientCapabilities, ClientKind, ContextParams, EventBody, HelloParams, Hint,
    InsertBufferParams, InsertMode, IpcClient, PROTOCOL_VERSION, RegisterSessionParams, Request,
    RequestBody, ResponseOutcome, ResponseResult, SafetyClassification, SessionContext, SessionId,
};
use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as TerminalEvent, EventStream, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use serde::{Deserialize, Serialize};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Parser, Debug)]
#[command(
    name = "aicoach-ui",
    version,
    about = "AI Terminal Coach interactive window"
)]
struct Args {
    /// Existing shell session. Defaults to the last focused terminal session.
    #[arg(long, default_value = "")]
    session: String,
    /// Override the Unix socket path (primarily for tests).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Mark a TUI launched by the native window controller.
    #[arg(long, hide = true)]
    managed_window: bool,
}

const IPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Speaker {
    User,
    Coach,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UiMessage {
    speaker: Speaker,
    content: String,
}

#[derive(Debug, Clone)]
struct CommandSuggestion {
    command: String,
    safety: SafetyClassification,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HistoryStore {
    #[serde(default)]
    sessions: BTreeMap<String, Vec<UiMessage>>,
    #[serde(default)]
    updated_at_micros: BTreeMap<String, i64>,
    #[serde(flatten)]
    additional_fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy)]
enum UiLanguage {
    English,
    Chinese,
}

impl UiLanguage {
    fn from_config(config: &Config) -> Self {
        if config.coach.language == "zh-CN" {
            Self::Chinese
        } else {
            Self::English
        }
    }

    const fn text(self, english: &'static str, chinese: &'static str) -> &'static str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }
}

struct App {
    session_id: SessionId,
    context: Option<SessionContext>,
    messages: Vec<UiMessage>,
    input: String,
    input_cursor: usize,
    recommendations: Vec<CommandSuggestion>,
    recommendation_state: ListState,
    safety: SafetyEngine,
    scroll: u16,
    follow_tail: bool,
    streaming: bool,
    status: String,
    should_quit: bool,
    show_help: bool,
    history_enabled: bool,
    history_limit: usize,
    history_path: PathBuf,
    managed_window: bool,
    language: UiLanguage,
}

impl App {
    fn new(
        session_id: SessionId,
        config: &Config,
        paths: &ProductPaths,
        managed_window: bool,
    ) -> Self {
        let language = UiLanguage::from_config(config);
        let safety = SafetyEngine::with_config(config.safety.clone());
        let mut recommendation_state = ListState::default();
        recommendation_state.select(None);
        let (mut messages, status) = if config.history.enabled {
            match load_history(&paths.history_file, session_id) {
                Ok(messages) => (
                    messages,
                    language
                        .text("Connected to daemon", "已连接后台服务")
                        .to_owned(),
                ),
                Err(error) => (
                    Vec::new(),
                    format!(
                        "{}: {error}",
                        language.text(
                            "History could not be read; the original file was preserved",
                            "历史记录无法读取，原文件已保留"
                        )
                    ),
                ),
            }
        } else {
            (
                Vec::new(),
                language
                    .text("Connected to daemon", "已连接后台服务")
                    .to_owned(),
            )
        };
        for message in &mut messages {
            message.content = sanitize_terminal_text(&message.content, true);
        }
        Self {
            session_id,
            context: None,
            messages,
            input: String::new(),
            input_cursor: 0,
            recommendations: Vec::new(),
            recommendation_state,
            safety,
            scroll: 0,
            follow_tail: true,
            streaming: false,
            status,
            should_quit: false,
            show_help: false,
            history_enabled: config.history.enabled,
            history_limit: config.history.max_messages,
            history_path: paths.history_file.clone(),
            managed_window,
            language,
        }
    }

    fn selected_suggestion(&self) -> Option<&CommandSuggestion> {
        self.recommendation_state
            .selected()
            .and_then(|index| self.recommendations.get(index))
    }

    fn add_recommendation(&mut self, command: impl Into<String>) {
        let command = command.into().trim().to_owned();
        if command.is_empty()
            || command.contains('\n')
            || command.chars().any(char::is_control)
            || self
                .recommendations
                .iter()
                .any(|item| item.command == command)
        {
            return;
        }
        let report = self.safety.risk_lens(&command);
        self.recommendations.push(CommandSuggestion {
            command,
            safety: SafetyClassification::from(&report),
        });
        if self.recommendations.len() > 12 {
            self.recommendations.remove(0);
        }
        self.recommendation_state
            .select(Some(self.recommendations.len() - 1));
    }

    fn push_assistant_delta(&mut self, delta: &str) {
        let delta = sanitize_terminal_text(delta, true);
        if let Some(last) = self
            .messages
            .last_mut()
            .filter(|message| matches!(message.speaker, Speaker::Coach))
        {
            last.content.push_str(&delta);
        } else {
            self.messages.push(UiMessage {
                speaker: Speaker::Coach,
                content: delta,
            });
        }
    }

    fn finish_stream(&mut self) {
        self.streaming = false;
        if let Some(content) = self
            .messages
            .last()
            .filter(|message| matches!(message.speaker, Speaker::Coach))
            .map(|message| message.content.clone())
        {
            for command in extract_commands(&content) {
                self.add_recommendation(command);
            }
        }
        self.persist_history();
    }

    fn persist_history(&mut self) {
        if !self.history_enabled {
            return;
        }
        if let Err(error) = save_history(
            &self.history_path,
            self.session_id,
            &self.messages,
            self.history_limit,
        ) {
            self.status = format!(
                "{}: {error}",
                self.language.text(
                    "History save failed; the original file was not overwritten",
                    "历史记录保存失败，原文件未覆盖"
                )
            );
        }
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable terminal raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)
            .context("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("\x1b[31mAI Coach UI:\x1b[0m {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();
    let paths = ProductPaths::discover()?;
    let config = Config::load_or_create()?;
    let socket = args.socket.unwrap_or_else(|| paths.socket_file.clone());
    let client = IpcClient::connect(&socket)
        .await
        .with_context(|| format!("connect to {}; run `aicoach start` first", socket.display()))?;
    hello(&client).await?;

    let requested = resolve_requested_session(&args.session, &paths.run_dir);
    let session_id = attach_session(&client, requested).await?;
    let mut app = App::new(session_id, &config, &paths, args.managed_window);
    refresh_context(&client, &mut app).await;

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("create terminal UI")?;
    terminal.clear()?;
    let mut terminal_events = EventStream::new();
    let mut ipc_events = client.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut context_tick = tokio::time::interval(Duration::from_secs(3));

    while !app.should_quit {
        terminal.draw(|frame| draw(frame, &mut app))?;
        tokio::select! {
            event = terminal_events.next() => {
                if let Some(Ok(event)) = event {
                    handle_terminal_event(event, &client, &mut app).await?;
                }
            }
            event = ipc_events.recv() => {
                match event {
                    Ok(event) if event.session_id == app.session_id => handle_ipc_event(event.body, &mut app),
                    Ok(_) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => app.status = format!(
                        "{} {count} {}",
                        app.language.text("Skipped", "跳过"),
                        app.language.text("stale events", "条过期事件")
                    ),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        app.status = app.language.text(
                            "Daemon connection closed; the terminal is unaffected",
                            "后台连接已关闭；终端本身不受影响"
                        ).to_owned();
                    }
                }
            }
            _ = context_tick.tick() => refresh_context(&client, &mut app).await,
            _ = tick.tick() => {}
        }
    }

    app.persist_history();
    client.close().await.ok();
    Ok(())
}

async fn hello(client: &IpcClient) -> Result<()> {
    let response = client
        .send_timeout(
            Request::new(
                None,
                RequestBody::Hello(HelloParams {
                    protocol_version: PROTOCOL_VERSION,
                    client_name: "aicoach-ui".to_owned(),
                    client_version: env!("CARGO_PKG_VERSION").to_owned(),
                    client_kind: ClientKind::Tui,
                    capabilities: ClientCapabilities {
                        push_events: true,
                        streaming: true,
                        insert_buffer: true,
                        shell_line_protocol: false,
                    },
                }),
            ),
            IPC_REQUEST_TIMEOUT,
        )
        .await?;
    match response.outcome {
        ResponseOutcome::Ok {
            result: ResponseResult::Hello {
                protocol_version, ..
            },
        } if protocol_version == PROTOCOL_VERSION => Ok(()),
        ResponseOutcome::Error { error } => bail!("daemon rejected handshake: {}", error.message),
        other => bail!("unexpected handshake response: {other:?}"),
    }
}

fn resolve_requested_session(argument: &str, run_dir: &Path) -> Option<SessionId> {
    if let Ok(session) = argument.parse() {
        return Some(session);
    }
    fs::read_to_string(run_dir.join("active-session"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
}

async fn attach_session(client: &IpcClient, requested: Option<SessionId>) -> Result<SessionId> {
    if let Some(session_id) = requested {
        let probe = client
            .send_timeout(
                Request::new(
                    Some(session_id),
                    RequestBody::Context(ContextParams {
                        max_commands: Some(1),
                    }),
                ),
                IPC_REQUEST_TIMEOUT,
            )
            .await?;
        if matches!(
            probe.outcome,
            ResponseOutcome::Ok {
                result: ResponseResult::Context(_)
            }
        ) {
            return Ok(session_id);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let response = client
        .send_timeout(
            Request::new(
                None,
                RequestBody::RegisterSession(RegisterSessionParams {
                    requested_session_id: requested,
                    tty: std::env::var("TTY").unwrap_or_else(|_| "/dev/tty".to_owned()),
                    pid: Some(std::process::id()),
                    cwd,
                    shell: "zsh".to_owned(),
                    terminal: std::env::var("TERM_PROGRAM").ok(),
                    environment: BTreeMap::new(),
                }),
            ),
            IPC_REQUEST_TIMEOUT,
        )
        .await?;
    match response.outcome {
        ResponseOutcome::Ok {
            result: ResponseResult::SessionRegistered { session_id },
        } => Ok(session_id),
        ResponseOutcome::Error { error } => bail!("cannot register TUI session: {}", error.message),
        other => bail!("unexpected registration response: {other:?}"),
    }
}

async fn refresh_context(client: &IpcClient, app: &mut App) {
    let result = client
        .send_timeout(
            Request::new(
                Some(app.session_id),
                RequestBody::Context(ContextParams {
                    max_commands: Some(20),
                }),
            ),
            IPC_REQUEST_TIMEOUT,
        )
        .await;
    match result {
        Ok(response) => match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::Context(context),
            } => app.context = Some(context),
            ResponseOutcome::Error { error } => {
                app.status = format!(
                    "{}: {}",
                    app.language.text("Context unavailable", "上下文不可用"),
                    error.message
                )
            }
            _ => {}
        },
        Err(error) => {
            app.status = format!(
                "{}: {error}",
                app.language
                    .text("Daemon temporarily unavailable", "后台暂不可用")
            )
        }
    }
}

async fn handle_terminal_event(
    event: TerminalEvent,
    client: &IpcClient,
    app: &mut App,
) -> Result<()> {
    match event {
        TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => {
            handle_key(key, client, app).await?
        }
        TerminalEvent::Paste(text) => {
            let text = sanitize_terminal_text(&text, true);
            insert_text(&mut app.input, &mut app.input_cursor, &text);
        }
        TerminalEvent::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => scroll_chat_up(app, 3),
            MouseEventKind::ScrollDown => scroll_chat_down(app, 3),
            _ => {}
        },
        TerminalEvent::Resize(_, _) | TerminalEvent::FocusGained | TerminalEvent::FocusLost => {}
        _ => {}
    }
    Ok(())
}

async fn handle_key(key: KeyEvent, client: &IpcClient, app: &mut App) -> Result<()> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'q'))
    {
        app.should_quit = true;
        return Ok(());
    }
    if is_insert_shortcut(key, app.input.is_empty()) {
        return insert_selected(client, app).await;
    }
    if is_copy_shortcut(key) {
        return copy_selected(app);
    }
    match key.code {
        KeyCode::Esc if app.show_help => app.show_help = false,
        KeyCode::Esc => {
            app.persist_history();
            return_to_terminal(app, None);
        }
        KeyCode::F(1) | KeyCode::Char('?') if app.input.is_empty() => {
            app.show_help = !app.show_help
        }
        KeyCode::Enter => submit_chat(client, app).await?,
        KeyCode::Backspace => remove_before_cursor(&mut app.input, &mut app.input_cursor),
        KeyCode::Delete => remove_at_cursor(&mut app.input, app.input_cursor),
        KeyCode::Left => app.input_cursor = app.input_cursor.saturating_sub(1),
        KeyCode::Right => app.input_cursor = (app.input_cursor + 1).min(app.input.chars().count()),
        KeyCode::Home => app.input_cursor = 0,
        KeyCode::End => app.input_cursor = app.input.chars().count(),
        KeyCode::Up if app.input.is_empty() => select_previous(app),
        KeyCode::Down if app.input.is_empty() => select_next(app),
        KeyCode::PageUp => scroll_chat_up(app, 5),
        KeyCode::PageDown => scroll_chat_down(app, 5),
        KeyCode::Char(character)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
        {
            insert_text(
                &mut app.input,
                &mut app.input_cursor,
                &character.to_string(),
            );
        }
        _ => {}
    }
    Ok(())
}

async fn submit_chat(client: &IpcClient, app: &mut App) -> Result<()> {
    let message = app.input.trim().to_owned();
    if message.is_empty() || app.streaming {
        return Ok(());
    }
    app.input.clear();
    app.input_cursor = 0;
    app.messages.push(UiMessage {
        speaker: Speaker::User,
        content: message.clone(),
    });
    app.messages.push(UiMessage {
        speaker: Speaker::Coach,
        content: String::new(),
    });
    app.follow_tail = true;
    app.streaming = true;
    app.status = app
        .language
        .text("AI is responding…", "AI 正在回答…")
        .to_owned();
    let cwd = app.context.as_ref().map(|context| context.cwd.clone());
    let response = client
        .send_timeout(
            Request::new(
                Some(app.session_id),
                RequestBody::Chat(ChatParams {
                    message,
                    stream: true,
                    cwd,
                    buffer: None,
                }),
            ),
            IPC_REQUEST_TIMEOUT,
        )
        .await;
    match response {
        Ok(response) => match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::Accepted,
            } => {}
            ResponseOutcome::Ok {
                result: ResponseResult::Chat { message },
            } => {
                app.push_assistant_delta(&message);
                app.finish_stream();
            }
            ResponseOutcome::Error { error } => {
                app.push_assistant_delta(&format!(
                    "{}: {}",
                    app.language
                        .text("AI service is temporarily unavailable", "AI 服务暂时不可用"),
                    error.message
                ));
                app.finish_stream();
            }
            _ => {
                app.push_assistant_delta(app.language.text(
                    "The daemon returned an unrecognized response.",
                    "后台返回了无法识别的响应。",
                ));
                app.finish_stream();
            }
        },
        Err(error) => {
            app.push_assistant_delta(&format!(
                "{}: {error}",
                app.language
                    .text("AI service is temporarily unavailable", "AI 服务暂时不可用")
            ));
            app.finish_stream();
        }
    }
    Ok(())
}

fn handle_ipc_event(event: EventBody, app: &mut App) {
    match event {
        EventBody::ChatDelta { delta } => app.push_assistant_delta(&delta),
        EventBody::ChatDone => {
            app.status = app
                .language
                .text("Response complete", "回答完成")
                .to_owned();
            app.finish_stream();
        }
        EventBody::ChatFailed { message, .. } => {
            app.push_assistant_delta(&format!(
                "\n{}: {message}",
                app.language
                    .text("AI service is temporarily unavailable", "AI 服务暂时不可用")
            ));
            app.status = app
                .language
                .text(
                    "Response interrupted; partial content was preserved",
                    "回答中断；已保留收到的部分内容",
                )
                .to_owned();
            app.finish_stream();
        }
        EventBody::Hint(hint) => handle_hint(hint, app),
        EventBody::Completion(completion) => {
            app.add_recommendation(completion.command);
            app.status = sanitize_terminal_text(
                &completion.description.unwrap_or_else(|| {
                    app.language
                        .text("New suggestion received", "收到新建议")
                        .to_owned()
                }),
                false,
            );
        }
        EventBody::InsertBuffer(_) => {
            app.status = app
                .language
                .text(
                    "Command sent to the original terminal input line",
                    "命令已发送到原终端输入行",
                )
                .to_owned()
        }
        EventBody::RequestCancelled => {
            app.streaming = false;
            app.status = app
                .language
                .text("Request cancelled", "请求已取消")
                .to_owned();
        }
        EventBody::DataCleared { scope } => {
            app.messages.clear();
            app.streaming = false;
            app.recommendations.clear();
            app.recommendation_state.select(None);
            app.scroll = 0;
            app.follow_tail = true;
            if !matches!(scope, aicoach_ipc::DataClearScope::ChatHistory) {
                app.context = None;
                app.input.clear();
                app.input_cursor = 0;
            }
            app.status = app
                .language
                .text("Local session data was cleared", "本地会话数据已清除")
                .to_owned();
            app.persist_history();
        }
        EventBody::SessionClosed => {
            app.status = app
                .language
                .text("The terminal session was closed", "对应终端会话已关闭")
                .to_owned()
        }
    }
}

fn handle_hint(hint: Hint, app: &mut App) {
    if let Some(command) = hint.suggested_command {
        app.add_recommendation(command);
    }
    app.messages.push(UiMessage {
        speaker: Speaker::System,
        content: sanitize_terminal_text(&format!("{}：{}", hint.title, hint.message), true),
    });
    app.status = app
        .language
        .text("Terminal analysis received", "收到终端分析")
        .to_owned();
}

async fn insert_selected(client: &IpcClient, app: &mut App) -> Result<()> {
    let Some((command, safety)) = app
        .selected_suggestion()
        .map(|suggestion| (suggestion.command.clone(), suggestion.safety))
    else {
        app.status = app
            .language
            .text("No command is available to insert", "当前没有可插入的命令")
            .to_owned();
        return Ok(());
    };
    let response = client
        .send_timeout(
            Request::new(
                Some(app.session_id),
                RequestBody::InsertBuffer(InsertBufferParams {
                    command,
                    cursor: None,
                    mode: InsertMode::Replace,
                    safety: None,
                }),
            ),
            IPC_REQUEST_TIMEOUT,
        )
        .await?;
    match response.outcome {
        ResponseOutcome::Ok { .. } => {
            let status = action_status(SuggestionAction::Insert, safety, app.language);
            return_to_terminal(app, Some(status));
        }
        ResponseOutcome::Error { error } => {
            app.status = format!(
                "{}: {}",
                app.language.text("Unable to insert", "无法插入"),
                error.message
            )
        }
    }
    Ok(())
}

fn return_to_terminal(app: &mut App, success_status: Option<String>) {
    if app.managed_window {
        match Command::new("aicoach")
            .args(["toggle", "--session", &app.session_id.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => {
                app.status = success_status.unwrap_or_else(|| {
                    app.language
                        .text(
                            "Returned to terminal; Coach keeps the context",
                            "已返回原终端；Coach 会继续保留上下文",
                        )
                        .to_owned()
                })
            }
            Err(error) => {
                app.status = format!(
                    "{}: {error}",
                    app.language.text("Unable to hide Coach", "无法隐藏 Coach")
                )
            }
        }
    } else {
        // A directly launched UI belongs to the current terminal. Exiting lets
        // the Zsh precmd/line-init hooks apply any queued buffer insertion.
        app.should_quit = true;
    }
}

fn copy_selected(app: &mut App) -> Result<()> {
    let Some((command, safety)) = app
        .selected_suggestion()
        .map(|suggestion| (suggestion.command.clone(), suggestion.safety))
    else {
        app.status = app
            .language
            .text("No command is available to copy", "当前没有可复制的命令")
            .to_owned();
        return Ok(());
    };
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("start pbcopy")?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| anyhow!("pbcopy stdin unavailable"))?
        .write_all(command.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        app.status = action_status(SuggestionAction::Copy, safety, app.language);
    } else {
        app.status = app
            .language
            .text("Clipboard helper failed", "剪贴板工具执行失败")
            .to_owned();
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SuggestionAction {
    Insert,
    Copy,
}

fn risk_rating(level: Option<RiskLevel>, language: UiLanguage) -> &'static str {
    match (level, language) {
        (Some(RiskLevel::Low), UiLanguage::English) => "LOW",
        (Some(RiskLevel::Medium), UiLanguage::English) => "MEDIUM",
        (Some(RiskLevel::High), UiLanguage::English) => "HIGH",
        (Some(RiskLevel::Critical), UiLanguage::English) => "CRITICAL",
        (None, UiLanguage::English) => "UNRATED",
        (Some(RiskLevel::Low), UiLanguage::Chinese) => "低风险",
        (Some(RiskLevel::Medium), UiLanguage::Chinese) => "中风险",
        (Some(RiskLevel::High), UiLanguage::Chinese) => "高风险",
        (Some(RiskLevel::Critical), UiLanguage::Chinese) => "严重风险",
        (None, UiLanguage::Chinese) => "未评级",
    }
}

fn safety_badge(safety: SafetyClassification, language: UiLanguage) -> String {
    let mut badge = risk_rating(safety.level, language).to_owned();
    if safety.coverage == AnalysisCoverage::Partial {
        badge.push_str(language.text("/PARTIAL", "/部分"));
    }
    if !safety.safety_rules_enabled {
        badge.push_str(language.text("/RULES OFF", "/规则关"));
    }
    format!("[{badge}]")
}

fn safety_summary(safety: SafetyClassification, language: UiLanguage) -> String {
    let mut parts = vec![risk_rating(safety.level, language)];
    match safety.coverage {
        AnalysisCoverage::Recognized => {}
        AnalysisCoverage::Partial => {
            parts.push(language.text("partial coverage", "部分识别"));
        }
        AnalysisCoverage::Unknown => {
            parts.push(language.text("unknown command", "未识别命令"));
        }
    }
    if !safety.safety_rules_enabled {
        parts.push(language.text("destructive rules off", "破坏性规则已关闭"));
    }
    parts.join(" · ")
}

fn safety_style(safety: SafetyClassification) -> Style {
    let color = match safety.level {
        Some(RiskLevel::Low) => Color::Green,
        Some(RiskLevel::Medium) | None => Color::Yellow,
        Some(RiskLevel::High) => Color::LightRed,
        Some(RiskLevel::Critical) => Color::Red,
    };
    let style = Style::default().fg(color);
    if safety.level.is_none_or(|level| level >= RiskLevel::High) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn action_status(
    action: SuggestionAction,
    safety: SafetyClassification,
    language: UiLanguage,
) -> String {
    let classification = safety_summary(safety, language);
    match (action, language) {
        (SuggestionAction::Insert, UiLanguage::English) => format!(
            "Insert only · {classification} · sent to the original terminal; review it and press Enter yourself"
        ),
        (SuggestionAction::Insert, UiLanguage::Chinese) => {
            format!("仅插入 · {classification} · 已发送到原终端；请检查后自行按 Enter")
        }
        (SuggestionAction::Copy, UiLanguage::English) => {
            format!("Copy only · {classification} · clipboard updated; nothing executed")
        }
        (SuggestionAction::Copy, UiLanguage::Chinese) => {
            format!("仅复制 · {classification} · 已写入剪贴板；未执行任何命令")
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let area = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    let cwd = app.context.as_ref().map_or_else(
        || {
            app.language
                .text("Waiting for context…", "等待上下文…")
                .to_owned()
        },
        |context| sanitize_terminal_text(&context.cwd.display().to_string(), false),
    );
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " AI Terminal Coach ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {cwd}"), Style::default().fg(Color::DarkGray)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );
    frame.render_widget(header, vertical[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(vertical[1]);
    draw_context(frame, body[0], app);
    draw_chat(frame, body[1], app);

    let input_title = if app.streaming {
        app.language.text("Question (waiting)", "提问（等待回答）")
    } else {
        app.language.text("Question", "提问")
    };
    let input = Paragraph::new(app.input.as_str()).block(
        Block::default()
            .title(input_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(input, vertical[2]);
    let input_prefix: String = app.input.chars().take(app.input_cursor).collect();
    let visible_cursor =
        u16::try_from(UnicodeWidthStr::width(input_prefix.as_str())).unwrap_or(u16::MAX);
    frame.set_cursor_position((
        vertical[2].x + 1 + visible_cursor.min(vertical[2].width.saturating_sub(2)),
        vertical[2].y + 1,
    ));

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::raw(app.language.text(" Return  ", " 返回终端  ")),
        Span::styled("⌥I/Tab", Style::default().fg(Color::Yellow)),
        Span::raw(
            app.language
                .text(" Insert only & return  ", " 仅插入并返回  "),
        ),
        Span::styled("⌥Y/⌃Y", Style::default().fg(Color::Yellow)),
        Span::raw(app.language.text(" Copy only  ", " 仅复制  ")),
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(app.language.text(" Select  ", " 选择  ")),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(app.language.text(" Help  ", " 帮助  ")),
        Span::styled("Ctrl-Q", Style::default().fg(Color::Yellow)),
        Span::raw(app.language.text(" Quit  ", " 退出  ")),
        Span::styled(&app.status, Style::default().fg(Color::DarkGray)),
    ]))
    .alignment(Alignment::Left);
    frame.render_widget(footer, vertical[3]);

    if app.show_help {
        draw_help(frame, centered_rect(70, 70, area), app.language);
    }
}

fn draw_context(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);
    let commands: Vec<ListItem<'_>> = app.context.as_ref().map_or_else(Vec::new, |context| {
        context
            .commands
            .iter()
            .rev()
            .take(12)
            .map(|command| {
                let color = if command.exit_code == 0 {
                    Color::Green
                } else {
                    Color::Red
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{} ", command.exit_code),
                        Style::default().fg(color),
                    ),
                    Span::raw(sanitize_terminal_text(&command.command, false)),
                ]))
            })
            .collect()
    });
    frame.render_widget(
        List::new(commands).block(
            Block::default()
                .title(app.language.text("Recent commands", "最近命令"))
                .borders(Borders::ALL),
        ),
        vertical[0],
    );

    let recommendations: Vec<ListItem<'_>> = app
        .recommendations
        .iter()
        .map(|suggestion| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", safety_badge(suggestion.safety, app.language)),
                    safety_style(suggestion.safety),
                ),
                Span::raw(suggestion.command.as_str()),
            ]))
        })
        .collect();
    let list = List::new(recommendations)
        .block(
            Block::default()
                .title(
                    app.language
                        .text("Suggestions (never auto-run)", "推荐（不会自动执行）"),
                )
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, vertical[1], &mut app.recommendation_state);
}

fn draw_chat(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    for message in &app.messages {
        let (label, color) = match message.speaker {
            Speaker::User => (app.language.text("You", "你"), Color::Green),
            Speaker::Coach => ("AI", Color::Cyan),
            Speaker::System => (app.language.text("Terminal", "终端"), Color::Yellow),
        };
        lines.push(Line::from(Span::styled(
            format!("{label}  "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        if message.content.is_empty() && app.streaming {
            lines.push(Line::from(Span::styled(
                "  ▌",
                Style::default().fg(Color::Cyan),
            )));
        } else {
            lines.extend(
                message
                    .content
                    .lines()
                    .flat_map(|line| wrap_display_line(&format!("  {line}"), inner_width))
                    .map(Line::from),
            );
        }
        lines.push(Line::default());
    }
    let content_height = lines.len();
    let paragraph = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .title(app.language.text("Conversation", "对话"))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );
    let viewport_height = usize::from(area.height.saturating_sub(2));
    let (scroll, follow_tail) =
        resolved_chat_scroll(app.scroll, app.follow_tail, content_height, viewport_height);
    app.scroll = scroll;
    app.follow_tail = follow_tail;
    let paragraph = paragraph.scroll((app.scroll, 0));
    frame.render_widget(paragraph, area);

    if content_height > viewport_height && viewport_height > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        let mut state = ScrollbarState::new(content_height)
            .position(usize::from(app.scroll))
            .viewport_content_length(viewport_height);
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

fn draw_help(frame: &mut ratatui::Frame<'_>, area: Rect, language: UiLanguage) {
    frame.render_widget(Clear, area);
    let text = Text::from(vec![
        Line::from(Span::styled(
            language.text("AI Terminal Coach shortcuts", "AI Terminal Coach 快捷键"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(language.text("Enter      Send question", "Enter      发送问题")),
        Line::from(language.text(
            "Esc        Hide Coach and return to terminal",
            "Esc        隐藏 Coach 并返回原终端",
        )),
        Line::from(language.text("↑ / ↓      Select suggestion", "↑ / ↓      选择推荐命令")),
        Line::from(language.text(
            "Option+I / Tab / F2  Insert only, return, then review and press Enter",
            "Option+I / Tab / F2  仅插入并返回；检查后仍需自行按 Enter",
        )),
        Line::from(language.text(
            "Option+Y / Ctrl+Y / F3 Copy only; never run the command",
            "Option+Y / Ctrl+Y / F3 仅复制；不会执行命令",
        )),
        Line::from(language.text(
            "PgUp/PgDn or mouse wheel  Scroll conversation",
            "PgUp/PgDn 或鼠标滚轮  滚动对话",
        )),
        Line::from(language.text("Ctrl+Q     Quit Coach", "Ctrl+Q     退出 Coach")),
        Line::default(),
        Line::from(Span::styled(
            language.text(
                "Every suggestion is locally rated before insert/copy; UNRATED is not safe.",
                "每条建议在插入/复制前都会本地评级；“未评级”不等于安全。",
            ),
            Style::default().fg(Color::Yellow),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .title(language.text("Help (? to close)", "帮助（? 关闭）"))
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn is_insert_shortcut(key: KeyEvent, input_is_empty: bool) -> bool {
    (key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('i' | 'I')))
        // Terminal.app emits this native dead-key glyph when Option is not
        // configured as Meta. F2/Tab remain reliable on every key layout.
        || matches!(key.code, KeyCode::Char('ˆ') | KeyCode::F(2))
        || (input_is_empty && key.code == KeyCode::Tab)
}

fn wrap_display_line(value: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut wrapped = Vec::new();
    let mut line = String::new();
    let mut line_width = 0usize;
    for character in value.chars() {
        let character_width = if character == '\t' {
            4
        } else {
            UnicodeWidthChar::width(character).unwrap_or_default()
        };
        if line_width > 0 && line_width.saturating_add(character_width) > width {
            wrapped.push(line);
            line = String::new();
            line_width = 0;
        }
        if character == '\t' {
            line.push_str("    ");
        } else {
            line.push(character);
        }
        line_width = line_width.saturating_add(character_width);
    }
    wrapped.push(line);
    wrapped
}

fn is_copy_shortcut(key: KeyEvent) -> bool {
    (key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Char('y' | 'Y')))
        || matches!(key.code, KeyCode::Char('¥') | KeyCode::F(3))
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('y' | 'Y')))
}

fn scroll_chat_up(app: &mut App, amount: u16) {
    app.follow_tail = false;
    app.scroll = app.scroll.saturating_sub(amount);
}

fn scroll_chat_down(app: &mut App, amount: u16) {
    app.follow_tail = false;
    app.scroll = app.scroll.saturating_add(amount);
}

fn resolved_chat_scroll(
    current: u16,
    follow_tail: bool,
    content_height: usize,
    viewport_height: usize,
) -> (u16, bool) {
    let max_scroll =
        u16::try_from(content_height.saturating_sub(viewport_height)).unwrap_or(u16::MAX);
    if follow_tail {
        (max_scroll, true)
    } else {
        let current = current.min(max_scroll);
        (current, current == max_scroll)
    }
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

fn select_previous(app: &mut App) {
    if app.recommendations.is_empty() {
        return;
    }
    let current = app.recommendation_state.selected().unwrap_or(0);
    app.recommendation_state
        .select(Some(current.saturating_sub(1)));
}

fn select_next(app: &mut App) {
    if app.recommendations.is_empty() {
        return;
    }
    let current = app.recommendation_state.selected().unwrap_or(0);
    app.recommendation_state
        .select(Some((current + 1).min(app.recommendations.len() - 1)));
}

fn insert_text(buffer: &mut String, cursor: &mut usize, text: &str) {
    let byte = char_to_byte(buffer, *cursor);
    buffer.insert_str(byte, text);
    *cursor += text.chars().count();
}

fn remove_before_cursor(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = char_to_byte(buffer, *cursor - 1);
    let end = char_to_byte(buffer, *cursor);
    buffer.replace_range(start..end, "");
    *cursor -= 1;
}

fn remove_at_cursor(buffer: &mut String, cursor: usize) {
    if cursor >= buffer.chars().count() {
        return;
    }
    let start = char_to_byte(buffer, cursor);
    let end = char_to_byte(buffer, cursor + 1);
    buffer.replace_range(start..end, "");
}

fn char_to_byte(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map_or(value.len(), |(offset, _)| offset)
}

fn extract_commands(content: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_fence = false;
    let mut fenced_lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_fence {
                if fenced_lines.len() == 1 {
                    commands.push(fenced_lines.remove(0));
                }
                fenced_lines.clear();
                in_fence = false;
            } else {
                in_fence = true;
            }
            continue;
        }
        if in_fence {
            if !trimmed.is_empty() && !matches!(trimmed, "bash" | "zsh" | "sh" | "shell") {
                fenced_lines.push(trimmed.to_owned());
            }
        } else if let Some(marker) = trimmed.find("$ ") {
            let command = trimmed[marker + 2..].trim();
            if !command.is_empty() {
                commands.push(command.to_owned());
            }
        }
    }
    commands
}

fn sanitize_terminal_text(value: &str, preserve_layout: bool) -> String {
    let sanitized = strip_terminal_sequences(value, true);
    if preserve_layout {
        sanitized
    } else {
        sanitized
            .chars()
            .filter(|character| !character.is_control())
            .collect()
    }
}

fn load_history(path: &Path, session_id: SessionId) -> Result<Vec<UiMessage>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut store = serde_json::from_str::<HistoryStore>(&content)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(store
        .sessions
        .remove(&session_id.to_string())
        .unwrap_or_default())
}

fn save_history(
    path: &Path,
    session_id: SessionId,
    messages: &[UiMessage],
    limit: usize,
) -> Result<()> {
    let mut store = match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str::<HistoryStore>(&content)
            .with_context(|| format!("parse existing {}", path.display()))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => HistoryStore::default(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let session_key = session_id.to_string();
    if messages.is_empty() {
        store.sessions.remove(&session_key);
        store.updated_at_micros.remove(&session_key);
        if store.sessions.is_empty()
            && store.updated_at_micros.is_empty()
            && store.additional_fields.is_empty()
        {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("remove {}", path.display()));
                }
            }
            return Ok(());
        }
    } else {
        let start = messages.len().saturating_sub(limit);
        store
            .sessions
            .insert(session_key.clone(), messages[start..].to_vec());
        store
            .updated_at_micros
            .insert(session_key, chrono::Utc::now().timestamp_micros());
    }
    // Bound inactive sessions too; terminal context isolation does not require
    // retaining chat forever.
    while store.sessions.len() > aicoach_core::MAX_PERSISTED_HISTORY_SESSIONS {
        let Some(key) = store
            .sessions
            .keys()
            .min_by_key(|key| {
                store
                    .updated_at_micros
                    .get(*key)
                    .copied()
                    .unwrap_or_default()
            })
            .cloned()
        else {
            break;
        };
        store.sessions.remove(&key);
        store.updated_at_micros.remove(&key);
    }
    let encoded = serde_json::to_vec_pretty(&store)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("history path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".history-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_language_defaults_to_english_and_supports_chinese() {
        let english = Config::default();
        assert!(matches!(
            UiLanguage::from_config(&english),
            UiLanguage::English
        ));
        assert_eq!(
            UiLanguage::from_config(&english).text("Connected", "已连接"),
            "Connected"
        );

        let mut chinese = Config::default();
        chinese.coach.language = "zh-CN".to_owned();
        assert_eq!(
            UiLanguage::from_config(&chinese).text("Connected", "已连接"),
            "已连接"
        );
    }

    #[test]
    fn unicode_editor_uses_character_offsets() {
        let mut value = "你x".to_owned();
        let mut cursor = 1;
        insert_text(&mut value, &mut cursor, "好");
        assert_eq!(value, "你好x");
        assert_eq!(cursor, 2);
        remove_before_cursor(&mut value, &mut cursor);
        assert_eq!(value, "你x");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn terminal_control_sequences_are_removed_from_display_text() {
        assert_eq!(
            sanitize_terminal_text("safe\u{1b}]52;c;owned\u{7}\nnext", true),
            "safe\nnext"
        );
        assert_eq!(sanitize_terminal_text("a\nb", false), "ab");
    }

    #[test]
    fn extracts_only_explicit_commands() {
        let answer = "建议：\n```bash\ngit pull origin main\n```\n或 $ git fetch";
        assert_eq!(
            extract_commands(answer),
            vec!["git pull origin main", "git fetch"]
        );
    }

    #[test]
    fn history_is_bounded_and_private() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let session = SessionId::new();
        let messages = (0..10)
            .map(|index| UiMessage {
                speaker: Speaker::User,
                content: index.to_string(),
            })
            .collect::<Vec<_>>();
        save_history(&path, session, &messages, 3).unwrap();
        let loaded = load_history(&path, session).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].content, "7");
    }

    #[test]
    fn malformed_history_is_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        fs::write(&path, "{broken").unwrap();
        let session = SessionId::new();
        assert!(load_history(&path, session).is_err());
        assert!(save_history(&path, session, &[], 3).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "{broken");
    }

    #[test]
    fn history_evicts_the_least_recently_updated_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let oldest = SessionId::new();
        let mut store = HistoryStore::default();
        for index in 0..20 {
            let session = if index == 0 { oldest } else { SessionId::new() };
            let key = session.to_string();
            store.sessions.insert(
                key.clone(),
                vec![UiMessage {
                    speaker: Speaker::User,
                    content: index.to_string(),
                }],
            );
            store.updated_at_micros.insert(key, i64::from(index));
        }
        fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
        let newest = SessionId::new();
        save_history(
            &path,
            newest,
            &[UiMessage {
                speaker: Speaker::User,
                content: "newest".to_owned(),
            }],
            3,
        )
        .unwrap();
        let saved: HistoryStore = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved.sessions.len(), 20);
        assert!(!saved.sessions.contains_key(&oldest.to_string()));
        assert!(saved.sessions.contains_key(&newest.to_string()));
    }

    #[test]
    fn multiline_code_blocks_are_not_split_into_partial_commands() {
        let answer = "```zsh\nfor file in *; do\n  echo $file\ndone\n```";
        assert!(extract_commands(answer).is_empty());
    }

    #[test]
    fn suggestion_actions_show_honest_local_safety_classification() {
        let engine = SafetyEngine::new();
        let low = SafetyClassification::from(&engine.risk_lens("git status"));
        let critical = SafetyClassification::from(&engine.risk_lens("rm -rf /"));
        let unknown = SafetyClassification::from(&engine.risk_lens("company-tool deploy"));
        let partial =
            SafetyClassification::from(&engine.risk_lens("git status && company-tool deploy"));

        assert_eq!(safety_badge(low, UiLanguage::English), "[LOW]");
        assert_eq!(safety_badge(critical, UiLanguage::Chinese), "[严重风险]");
        assert_eq!(safety_badge(unknown, UiLanguage::English), "[UNRATED]");
        assert_eq!(
            safety_badge(partial, UiLanguage::English),
            "[UNRATED/PARTIAL]"
        );
        assert!(
            action_status(SuggestionAction::Copy, unknown, UiLanguage::English)
                .contains("unknown command · clipboard updated; nothing executed")
        );
        assert!(
            action_status(SuggestionAction::Insert, critical, UiLanguage::Chinese)
                .contains("已发送到原终端；请检查后自行按 Enter")
        );
    }

    #[test]
    fn shortcuts_accept_meta_native_and_terminal_safe_fallbacks() {
        assert!(is_insert_shortcut(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT),
            false
        ));
        assert!(is_insert_shortcut(
            KeyEvent::new(KeyCode::Char('ˆ'), KeyModifiers::NONE),
            false
        ));
        assert!(is_insert_shortcut(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            true
        ));
        assert!(!is_insert_shortcut(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            false
        ));
        assert!(is_copy_shortcut(KeyEvent::new(
            KeyCode::Char('¥'),
            KeyModifiers::NONE
        )));
        assert!(is_copy_shortcut(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn chat_scroll_follows_tail_and_preserves_manual_history_view() {
        assert_eq!(resolved_chat_scroll(0, true, 30, 10), (20, true));
        assert_eq!(resolved_chat_scroll(7, false, 30, 10), (7, false));
        assert_eq!(resolved_chat_scroll(99, false, 30, 10), (20, true));
        assert_eq!(resolved_chat_scroll(5, true, 4, 10), (0, true));
    }

    #[test]
    fn chat_lines_wrap_by_terminal_cell_width() {
        assert_eq!(wrap_display_line("你好ab", 4), vec!["你好", "ab"]);
        assert_eq!(wrap_display_line("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap_display_line("", 4), vec![""]);
    }

    #[test]
    fn data_clear_event_drops_open_window_history() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ProductPaths::from_home(directory.path());
        fs::create_dir_all(paths.history_file.parent().unwrap()).unwrap();
        let config = Config::default();
        let mut app = App::new(SessionId::new(), &config, &paths, false);
        app.messages.push(UiMessage {
            speaker: Speaker::User,
            content: "private chat".to_owned(),
        });
        app.persist_history();
        assert!(paths.history_file.exists());
        app.add_recommendation("echo private");
        app.streaming = true;

        handle_ipc_event(
            EventBody::DataCleared {
                scope: aicoach_ipc::DataClearScope::Session,
            },
            &mut app,
        );

        assert!(app.messages.is_empty());
        assert!(app.recommendations.is_empty());
        assert!(!app.streaming);
        assert!(app.status.contains("cleared"));
        assert!(!paths.history_file.exists());
    }
}
