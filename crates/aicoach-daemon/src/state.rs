use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use aicoach_ipc::{
    CommandFinishedParams, CommandId, CommandStartedParams, ContextCommand, RegisterSessionParams,
    RequestId, SessionContext, SessionId, sanitize_shell_environment,
};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub Uuid);

impl ConnectionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveRequestKind {
    Completion,
    Analysis,
    Chat,
}

#[derive(Debug, Clone)]
pub struct SessionLimits {
    pub max_commands: usize,
    pub max_output_per_command: usize,
    pub max_total_chars: usize,
    pub history_enabled: bool,
    pub max_chat_messages: usize,
    pub max_sessions: usize,
    pub disconnected_session_ttl: Duration,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_commands: 30,
            max_output_per_command: 20_000,
            max_total_chars: 100_000,
            history_enabled: true,
            max_chat_messages: 50,
            max_sessions: 64,
            disconnected_session_ttl: Duration::from_secs(60 * 60),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnalysisJob {
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub command_id: CommandId,
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: i32,
    pub duration_ms: Option<u64>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub screen_tail: Option<String>,
    pub current_environment: BTreeMap<String, String>,
    pub environment_changes: BTreeMap<String, Option<String>>,
    pub context: Vec<ContextCommand>,
}

#[derive(Debug)]
struct StartedCommand {
    command: String,
    cwd: PathBuf,
}

#[derive(Debug)]
struct ActiveRequest {
    kind: ActiveRequestKind,
    cancellation: CancellationToken,
}

#[derive(Debug)]
struct Session {
    id: SessionId,
    tty: String,
    pid: Option<u32>,
    cwd: PathBuf,
    shell: String,
    terminal: Option<String>,
    environment: BTreeMap<String, String>,
    connection_id: Option<ConnectionId>,
    started: HashMap<CommandId, StartedCommand>,
    commands: VecDeque<ContextCommand>,
    chat: VecDeque<(bool, String)>,
    active: HashMap<RequestId, ActiveRequest>,
    last_accessed: Instant,
}

#[derive(Debug, Default)]
struct State {
    sessions: HashMap<SessionId, Session>,
    focused: Option<SessionId>,
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    state: Arc<Mutex<State>>,
    limits: SessionLimits,
}

impl SessionManager {
    pub fn new(limits: SessionLimits) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            limits,
        }
    }

    pub fn register(
        &self,
        connection_id: ConnectionId,
        params: RegisterSessionParams,
    ) -> SessionId {
        let session_id = params.requested_session_id.unwrap_or_default();
        let environment = sanitize_shell_environment(params.environment);
        let mut state = self.state.lock();
        if !state.sessions.contains_key(&session_id) {
            prune_for_new_session(&mut state, &self.limits);
        }
        let now = Instant::now();
        let session = state.sessions.entry(session_id).or_insert_with(|| Session {
            id: session_id,
            tty: params.tty.clone(),
            pid: params.pid,
            cwd: params.cwd.clone(),
            shell: params.shell.clone(),
            terminal: params.terminal.clone(),
            environment: environment.clone(),
            connection_id: Some(connection_id),
            started: HashMap::new(),
            commands: VecDeque::new(),
            chat: VecDeque::new(),
            active: HashMap::new(),
            last_accessed: now,
        });
        session.tty = params.tty;
        session.pid = params.pid;
        session.cwd = params.cwd;
        session.shell = params.shell;
        session.terminal = params.terminal;
        // An empty map is also what an older client deserializes to, so treat
        // it as "no environment update" when reconnecting an existing session.
        if !environment.is_empty() {
            session.environment = environment;
        }
        session.connection_id = Some(connection_id);
        session.last_accessed = now;
        state.focused = Some(session_id);
        session_id
    }

    /// Creates a context-only session without assigning a shell connection.
    /// Existing shell-owned sessions are never detached or overwritten.
    pub fn register_detached(&self, params: RegisterSessionParams) -> SessionId {
        let session_id = params.requested_session_id.unwrap_or_default();
        let environment = sanitize_shell_environment(params.environment);
        let mut state = self.state.lock();
        if !state.sessions.contains_key(&session_id) {
            prune_for_new_session(&mut state, &self.limits);
        }
        let now = Instant::now();
        state.sessions.entry(session_id).or_insert_with(|| Session {
            id: session_id,
            tty: params.tty,
            pid: params.pid,
            cwd: params.cwd,
            shell: params.shell,
            terminal: params.terminal,
            environment,
            connection_id: None,
            started: HashMap::new(),
            commands: VecDeque::new(),
            chat: VecDeque::new(),
            active: HashMap::new(),
            last_accessed: now,
        });
        if let Some(session) = state.sessions.get_mut(&session_id) {
            session.last_accessed = now;
        }
        session_id
    }

    pub fn focus(&self, session_id: SessionId, tty: &str) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        if session.tty != tty {
            return false;
        }
        session.last_accessed = Instant::now();
        state.focused = Some(session_id);
        true
    }

    pub fn focused(&self) -> Option<SessionId> {
        self.state.lock().focused
    }

    pub fn connection_for(&self, session_id: SessionId) -> Option<ConnectionId> {
        self.state
            .lock()
            .sessions
            .get(&session_id)
            .and_then(|session| session.connection_id)
    }

    pub fn start_command(&self, session_id: SessionId, params: CommandStartedParams) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        session.last_accessed = Instant::now();
        session.cwd.clone_from(&params.cwd);
        session.started.insert(
            params.command_id,
            StartedCommand {
                command: params.command,
                cwd: params.cwd,
            },
        );
        true
    }

    pub fn finish_command(
        &self,
        session_id: SessionId,
        request_id: RequestId,
        params: CommandFinishedParams,
        screen_tail: Option<String>,
    ) -> Option<AnalysisJob> {
        let mut state = self.state.lock();
        let session = state.sessions.get_mut(&session_id)?;
        session.last_accessed = Instant::now();
        let next_environment = sanitize_shell_environment(params.environment);
        let started = session.started.remove(&params.command_id);
        let command = params
            .command
            .or_else(|| started.as_ref().map(|value| value.command.clone()))?;
        let cwd = params
            .cwd
            .or_else(|| started.map(|value| value.cwd))
            .unwrap_or_else(|| session.cwd.clone());
        session.cwd.clone_from(&cwd);

        // A legacy FINISH frame has no environment field and decodes to an
        // empty map. Preserve the last known snapshot in that case.
        let environment_changes = if next_environment.is_empty() {
            BTreeMap::new()
        } else {
            let changes = environment_changes(&session.environment, &next_environment);
            session.environment = next_environment;
            changes
        };
        let current_environment = session.environment.clone();

        let stderr = params
            .stderr
            .map(|value| truncate_tail(&value, self.limits.max_output_per_command));
        let stdout_budget = self
            .limits
            .max_output_per_command
            .saturating_sub(stderr.as_deref().map_or(0, |value| value.chars().count()));
        let stdout = params
            .stdout
            .map(|value| truncate_tail(&value, stdout_budget));
        let mut record = ContextCommand {
            command_id: params.command_id,
            command: truncate_tail(&command, self.limits.max_output_per_command),
            cwd: cwd.clone(),
            exit_code: params.exit_code,
            duration_ms: params.duration_ms,
            stdout_summary: stdout.clone(),
            stderr_summary: stderr.clone(),
        };
        bound_record(&mut record, self.limits.max_total_chars);
        session.commands.push_back(record);
        trim_context(session, &self.limits);

        Some(AnalysisJob {
            session_id,
            request_id,
            command_id: params.command_id,
            command,
            cwd,
            exit_code: params.exit_code,
            duration_ms: params.duration_ms,
            stdout,
            stderr,
            screen_tail: screen_tail
                .map(|value| truncate_tail(&value, self.limits.max_output_per_command)),
            current_environment,
            environment_changes,
            context: session.commands.iter().cloned().collect(),
        })
    }

    /// Attach a best-effort screen tail to the matching context record after
    /// asynchronous Terminal.app/iTerm2 capture completes.
    pub fn record_screen_tail(
        &self,
        session_id: SessionId,
        command_id: CommandId,
        screen_tail: &str,
    ) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        session.last_accessed = Instant::now();
        let Some(record) = session
            .commands
            .iter_mut()
            .rev()
            .find(|record| record.command_id == command_id)
        else {
            return false;
        };
        if record.stdout_summary.as_deref().is_none_or(str::is_empty)
            && record.stderr_summary.as_deref().is_none_or(str::is_empty)
        {
            record.stderr_summary = Some(truncate_tail(
                screen_tail,
                self.limits.max_output_per_command,
            ));
            bound_record(record, self.limits.max_total_chars);
            trim_context(session, &self.limits);
        }
        true
    }

    pub fn context(
        &self,
        session_id: SessionId,
        max_commands: Option<usize>,
    ) -> Option<SessionContext> {
        let mut state = self.state.lock();
        let session = state.sessions.get_mut(&session_id)?;
        session.last_accessed = Instant::now();
        let take = max_commands
            .unwrap_or(self.limits.max_commands)
            .min(self.limits.max_commands);
        let skip = session.commands.len().saturating_sub(take);
        Some(SessionContext {
            session_id: session.id,
            tty: session.tty.clone(),
            cwd: session.cwd.clone(),
            shell: session.shell.clone(),
            environment: session.environment.clone(),
            commands: session.commands.iter().skip(skip).cloned().collect(),
        })
    }

    pub fn terminal_info(&self, session_id: SessionId) -> Option<(String, Option<String>)> {
        let mut state = self.state.lock();
        state.sessions.get_mut(&session_id).map(|session| {
            session.last_accessed = Instant::now();
            (session.tty.clone(), session.terminal.clone())
        })
    }

    /// Returns `(is_user, content)` chat messages for provider context.
    pub fn chat_history(&self, session_id: SessionId) -> Option<Vec<(bool, String)>> {
        let mut state = self.state.lock();
        state.sessions.get_mut(&session_id).map(|session| {
            session.last_accessed = Instant::now();
            session.chat.iter().cloned().collect()
        })
    }

    pub fn push_chat(&self, session_id: SessionId, is_user: bool, content: String) -> bool {
        if !self.limits.history_enabled {
            return self.state.lock().sessions.contains_key(&session_id);
        }
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        session.last_accessed = Instant::now();
        session.chat.push_back((is_user, content));
        while session.chat.len() > self.limits.max_chat_messages {
            session.chat.pop_front();
        }
        true
    }

    pub fn begin_request(
        &self,
        session_id: SessionId,
        request_id: RequestId,
        kind: ActiveRequestKind,
    ) -> Option<(CancellationToken, Option<RequestId>)> {
        let mut state = self.state.lock();
        let session = state.sessions.get_mut(&session_id)?;
        session.last_accessed = Instant::now();
        let mut superseded = None;
        if kind == ActiveRequestKind::Completion {
            if let Some((old_id, old)) = session
                .active
                .iter()
                .find(|(_, active)| active.kind == ActiveRequestKind::Completion)
                .map(|(id, active)| (*id, active.cancellation.clone()))
            {
                old.cancel();
                session.active.remove(&old_id);
                superseded = Some(old_id);
            }
        }
        let cancellation = CancellationToken::new();
        session.active.insert(
            request_id,
            ActiveRequest {
                kind,
                cancellation: cancellation.clone(),
            },
        );
        Some((cancellation, superseded))
    }

    pub fn end_request(&self, session_id: SessionId, request_id: RequestId) {
        if let Some(session) = self.state.lock().sessions.get_mut(&session_id) {
            session.active.remove(&request_id);
            session.last_accessed = Instant::now();
        }
    }

    pub fn cancel_request(&self, session_id: SessionId, request_id: RequestId) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        session.last_accessed = Instant::now();
        let Some(active) = session.active.remove(&request_id) else {
            return false;
        };
        active.cancellation.cancel();
        true
    }

    pub fn detach_connection(&self, connection_id: ConnectionId) -> Vec<SessionId> {
        let mut state = self.state.lock();
        let mut detached = Vec::new();
        for session in state.sessions.values_mut() {
            if session.connection_id == Some(connection_id) {
                session.connection_id = None;
                session.last_accessed = Instant::now();
                detached.push(session.id);
            }
        }
        detached
    }

    pub fn detach_session(&self, session_id: SessionId, connection_id: ConnectionId) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        if session.connection_id != Some(connection_id) {
            return false;
        }
        session.connection_id = None;
        session.last_accessed = Instant::now();
        if state.focused == Some(session_id) {
            state.focused = None;
        }
        true
    }

    #[cfg(test)]
    pub fn active_request_count(&self, session_id: SessionId) -> usize {
        self.state
            .lock()
            .sessions
            .get(&session_id)
            .map_or(0, |session| session.active.len())
    }

    #[cfg(test)]
    pub fn session_count(&self) -> usize {
        self.state.lock().sessions.len()
    }
}

fn prune_for_new_session(state: &mut State, limits: &SessionLimits) {
    let now = Instant::now();
    state.sessions.retain(|_, session| {
        session.connection_id.is_some()
            || !session.active.is_empty()
            || now.duration_since(session.last_accessed) < limits.disconnected_session_ttl
    });
    while state.sessions.len() >= limits.max_sessions.max(1) {
        let candidate = state
            .sessions
            .iter()
            .filter(|(_, session)| session.connection_id.is_none() && session.active.is_empty())
            .min_by_key(|(_, session)| session.last_accessed)
            .map(|(session_id, _)| *session_id);
        let Some(session_id) = candidate else {
            break;
        };
        state.sessions.remove(&session_id);
        if state.focused == Some(session_id) {
            state.focused = None;
        }
    }
    if state
        .focused
        .is_some_and(|session_id| !state.sessions.contains_key(&session_id))
    {
        state.focused = None;
    }
}

fn environment_changes(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> BTreeMap<String, Option<String>> {
    let mut changes = BTreeMap::new();
    for key in previous.keys() {
        if !current.contains_key(key) {
            changes.insert(key.clone(), None);
        }
    }
    for (key, value) in current {
        if previous.get(key) != Some(value) {
            changes.insert(key.clone(), Some(value.clone()));
        }
    }
    changes
}

fn trim_context(session: &mut Session, limits: &SessionLimits) {
    while session.commands.len() > limits.max_commands {
        session.commands.pop_front();
    }
    let mut total = session.commands.iter().map(record_chars).sum::<usize>();
    while total > limits.max_total_chars {
        if let Some(removed) = session.commands.pop_front() {
            total = total.saturating_sub(record_chars(&removed));
        } else {
            break;
        }
    }
}

fn bound_record(record: &mut ContextCommand, max_chars: usize) {
    let cwd_chars = record.cwd.to_string_lossy().chars().count();
    let mut remaining = max_chars.saturating_sub(cwd_chars);
    record.command = truncate_tail(&record.command, remaining);
    remaining = remaining.saturating_sub(record.command.chars().count());

    if let Some(stderr) = record.stderr_summary.as_mut() {
        *stderr = truncate_tail(stderr, remaining);
        remaining = remaining.saturating_sub(stderr.chars().count());
    }
    if let Some(stdout) = record.stdout_summary.as_mut() {
        *stdout = truncate_tail(stdout, remaining);
    }
}

fn record_chars(record: &ContextCommand) -> usize {
    record.command.chars().count()
        + record.cwd.to_string_lossy().chars().count()
        + record
            .stdout_summary
            .as_deref()
            .map_or(0, |value| value.chars().count())
        + record
            .stderr_summary
            .as_deref()
            .map_or(0, |value| value.chars().count())
}

pub fn truncate_tail(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_owned();
    }
    let start = value
        .char_indices()
        .nth(char_count - max_chars)
        .map_or(0, |(index, _)| index);
    value[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(session: SessionId) -> RegisterSessionParams {
        RegisterSessionParams {
            requested_session_id: Some(session),
            tty: "/dev/ttys001".to_owned(),
            pid: Some(42),
            cwd: PathBuf::from("/tmp/a"),
            shell: "zsh".to_owned(),
            terminal: Some("Apple_Terminal".to_owned()),
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn sessions_remain_isolated() {
        let manager = SessionManager::new(SessionLimits::default());
        let a = SessionId::new();
        let b = SessionId::new();
        manager.register(ConnectionId::new(), registration(a));
        manager.register(ConnectionId::new(), registration(b));
        let command_id = CommandId::new();
        manager.start_command(
            a,
            CommandStartedParams {
                command_id,
                command: "false".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        );
        manager.finish_command(
            a,
            RequestId::new(),
            CommandFinishedParams {
                command_id,
                command: None,
                cwd: None,
                exit_code: 1,
                stdout: None,
                stderr: Some("failure".to_owned()),
                duration_ms: None,
                environment: BTreeMap::new(),
            },
            None,
        );
        assert_eq!(manager.context(a, None).unwrap().commands.len(), 1);
        assert!(manager.context(b, None).unwrap().commands.is_empty());
    }

    #[test]
    fn newer_completion_cancels_the_previous_one() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        manager.register(ConnectionId::new(), registration(session));
        let first = RequestId::new();
        let first_token = manager
            .begin_request(session, first, ActiveRequestKind::Completion)
            .unwrap()
            .0;
        let second = RequestId::new();
        let (_, superseded) = manager
            .begin_request(session, second, ActiveRequestKind::Completion)
            .unwrap();
        assert_eq!(superseded, Some(first));
        assert!(first_token.is_cancelled());
        assert_eq!(manager.active_request_count(session), 1);
    }

    #[test]
    fn disconnected_sessions_are_lru_bounded() {
        let manager = SessionManager::new(SessionLimits {
            max_sessions: 2,
            disconnected_session_ttl: Duration::from_secs(60),
            ..SessionLimits::default()
        });
        let first = SessionId::new();
        let first_connection = ConnectionId::new();
        manager.register(first_connection, registration(first));
        assert!(manager.detach_session(first, first_connection));

        let second = SessionId::new();
        let second_connection = ConnectionId::new();
        manager.register(second_connection, registration(second));
        assert!(manager.detach_session(second, second_connection));

        let third = SessionId::new();
        manager.register(ConnectionId::new(), registration(third));
        assert_eq!(manager.session_count(), 2);
        assert!(manager.context(first, None).is_none());
        assert!(manager.context(second, None).is_some());
        assert!(manager.context(third, None).is_some());
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate_tail("a你好", 2), "你好");
        assert_eq!(truncate_tail("sensitive", 0), "");
    }

    #[test]
    fn output_and_total_context_budgets_are_strict() {
        let limits = SessionLimits {
            max_commands: 30,
            max_output_per_command: 10,
            max_total_chars: 32,
            ..SessionLimits::default()
        };
        let manager = SessionManager::new(limits);
        let session = SessionId::new();
        manager.register(ConnectionId::new(), registration(session));
        let command_id = CommandId::new();
        manager.start_command(
            session,
            CommandStartedParams {
                command_id,
                command: "very-long-command".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        );
        manager.finish_command(
            session,
            RequestId::new(),
            CommandFinishedParams {
                command_id,
                command: None,
                cwd: None,
                exit_code: 1,
                stdout: Some("0123456789".to_owned()),
                stderr: Some("abcdefghij".to_owned()),
                duration_ms: Some(42),
                environment: BTreeMap::new(),
            },
            None,
        );
        let context = manager.context(session, None).unwrap();
        let record = context.commands.last().unwrap();
        let output_chars = record
            .stdout_summary
            .as_deref()
            .map_or(0, |value| value.chars().count())
            + record
                .stderr_summary
                .as_deref()
                .map_or(0, |value| value.chars().count());
        assert!(output_chars <= 10);
        assert!(record_chars(record) <= 32);
        assert_eq!(record.duration_ms, Some(42));
    }

    #[test]
    fn captured_screen_tail_is_added_to_the_matching_record() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        manager.register(ConnectionId::new(), registration(session));
        let command_id = CommandId::new();
        manager.start_command(
            session,
            CommandStartedParams {
                command_id,
                command: "false".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        );
        manager.finish_command(
            session,
            RequestId::new(),
            CommandFinishedParams {
                command_id,
                command: None,
                cwd: None,
                exit_code: 1,
                stdout: None,
                stderr: None,
                duration_ms: None,
                environment: BTreeMap::new(),
            },
            None,
        );
        assert!(manager.record_screen_tail(session, command_id, "captured failure"));
        assert_eq!(
            manager.context(session, None).unwrap().commands[0]
                .stderr_summary
                .as_deref(),
            Some("captured failure")
        );
    }

    #[test]
    fn tracks_only_allowlisted_environment_changes() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        let mut params = registration(session);
        params.environment = BTreeMap::from([
            ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
            ("VIRTUAL_ENV".to_owned(), "/tmp/old".to_owned()),
            ("API_TOKEN".to_owned(), "secret".to_owned()),
        ]);
        manager.register(ConnectionId::new(), params);
        let command_id = CommandId::new();
        manager.start_command(
            session,
            CommandStartedParams {
                command_id,
                command: "source .venv/bin/activate".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        );

        let job = manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id,
                    command: None,
                    cwd: None,
                    exit_code: 0,
                    stdout: None,
                    stderr: None,
                    duration_ms: Some(1),
                    environment: BTreeMap::from([
                        ("LANG".to_owned(), "zh_CN.UTF-8".to_owned()),
                        ("CONDA_DEFAULT_ENV".to_owned(), "dev".to_owned()),
                        ("AWS_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned()),
                    ]),
                },
                None,
            )
            .unwrap();

        assert_eq!(
            job.current_environment,
            BTreeMap::from([
                ("CONDA_DEFAULT_ENV".to_owned(), "dev".to_owned()),
                ("LANG".to_owned(), "zh_CN.UTF-8".to_owned()),
            ])
        );
        assert_eq!(
            job.environment_changes,
            BTreeMap::from([
                ("CONDA_DEFAULT_ENV".to_owned(), Some("dev".to_owned())),
                ("LANG".to_owned(), Some("zh_CN.UTF-8".to_owned())),
                ("VIRTUAL_ENV".to_owned(), None),
            ])
        );
        let context = manager.context(session, None).unwrap();
        assert_eq!(context.environment, job.current_environment);
        assert!(!context.environment.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!context.environment.contains_key("API_TOKEN"));
    }
}
