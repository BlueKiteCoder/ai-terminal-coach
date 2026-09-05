use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use aicoach_core::{EnvironmentSnapshot, GitContext, strip_terminal_sequences};
use aicoach_ipc::{
    CommandFinishedParams, CommandId, CommandStartedParams, ContextCommand, DataRemovalSummary,
    RegisterSessionParams, RequestId, SessionCheckpoint, SessionContext, SessionDataLimits,
    SessionDataSummary, SessionId, sanitize_shell_environment,
};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_CHECKPOINT_NAME_CHARS: usize = 120;
const MAX_CHECKPOINT_RESOLUTION_CHARS: usize = 2_000;

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
    pub last_success: Option<SuccessfulCommandBaseline>,
    pub context: Vec<ContextCommand>,
}

#[derive(Debug)]
pub enum FinishCommand {
    Recorded(Box<AnalysisJob>),
    Discarded,
}

impl FinishCommand {
    pub fn into_recorded(self) -> Option<AnalysisJob> {
        match self {
            Self::Recorded(job) => Some(*job),
            Self::Discarded => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessfulCommandBaseline {
    pub command_id: CommandId,
    pub snapshot: EnvironmentSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointError {
    SessionNotFound,
    EmptyName,
    EmptyResolution,
    NoActiveCheckpoint,
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
    discarded_commands: HashSet<CommandId>,
    commands: VecDeque<ContextCommand>,
    chat: VecDeque<(bool, String)>,
    active: HashMap<RequestId, ActiveRequest>,
    checkpoint: Option<SessionCheckpoint>,
    last_success: Option<SuccessfulCommandBaseline>,
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
            discarded_commands: HashSet::new(),
            commands: VecDeque::new(),
            chat: VecDeque::new(),
            active: HashMap::new(),
            checkpoint: None,
            last_success: None,
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
            discarded_commands: HashSet::new(),
            commands: VecDeque::new(),
            chat: VecDeque::new(),
            active: HashMap::new(),
            checkpoint: None,
            last_success: None,
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
    ) -> Option<FinishCommand> {
        let mut state = self.state.lock();
        let session = state.sessions.get_mut(&session_id)?;
        session.last_accessed = Instant::now();
        if session.discarded_commands.remove(&params.command_id) {
            return Some(FinishCommand::Discarded);
        }
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
        if params.exit_code == 0 {
            session.last_success = Some(SuccessfulCommandBaseline {
                command_id: params.command_id,
                snapshot: EnvironmentSnapshot::new(&cwd, current_environment.clone()),
            });
        }
        let last_success = session.last_success.clone();

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

        Some(FinishCommand::Recorded(Box::new(AnalysisJob {
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
            last_success,
            context: session.commands.iter().cloned().collect(),
        })))
    }

    /// Enrich the most recent successful command with an asynchronous Git
    /// probe. A stale probe can never overwrite a newer success baseline.
    pub fn record_success_git(
        &self,
        session_id: SessionId,
        command_id: CommandId,
        observed: bool,
        git: Option<GitContext>,
    ) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return false;
        };
        let Some(baseline) = session.last_success.as_mut() else {
            return false;
        };
        if baseline.command_id != command_id {
            return false;
        }
        baseline.snapshot.git_observed = observed;
        baseline.snapshot.git = git;
        true
    }

    pub fn start_checkpoint(
        &self,
        session_id: SessionId,
        name: &str,
        started_at_unix_ms: u64,
        exclude_active_command: bool,
    ) -> Result<SessionCheckpoint, CheckpointError> {
        let name = bounded_checkpoint_text(name, false, MAX_CHECKPOINT_NAME_CHARS)
            .ok_or(CheckpointError::EmptyName)?;
        let mut state = self.state.lock();
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CheckpointError::SessionNotFound)?;
        let checkpoint = SessionCheckpoint {
            name,
            started_at_unix_ms,
            started_after_command_id: session.commands.back().map(|command| command.command_id),
            start_command_id: exclude_active_command
                .then(|| sole_started_command_id(session))
                .flatten(),
            resolution: None,
            resolved_at_unix_ms: None,
            resolved_after_command_id: None,
            resolution_command_id: None,
        };
        session.checkpoint = Some(checkpoint.clone());
        session.last_accessed = Instant::now();
        Ok(checkpoint)
    }

    pub fn resolve_checkpoint(
        &self,
        session_id: SessionId,
        resolution: &str,
        resolved_at_unix_ms: u64,
        exclude_active_command: bool,
    ) -> Result<SessionCheckpoint, CheckpointError> {
        let resolution = bounded_checkpoint_text(resolution, true, MAX_CHECKPOINT_RESOLUTION_CHARS)
            .ok_or(CheckpointError::EmptyResolution)?;
        let mut state = self.state.lock();
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CheckpointError::SessionNotFound)?;
        let resolved_after_command_id = session.commands.back().map(|command| command.command_id);
        let resolution_command_id = exclude_active_command
            .then(|| sole_started_command_id(session))
            .flatten();
        let checkpoint = session
            .checkpoint
            .as_mut()
            .ok_or(CheckpointError::NoActiveCheckpoint)?;
        checkpoint.resolution = Some(resolution);
        checkpoint.resolved_at_unix_ms = Some(resolved_at_unix_ms);
        checkpoint.resolved_after_command_id = resolved_after_command_id;
        checkpoint.resolution_command_id = resolution_command_id;
        session.last_accessed = Instant::now();
        Ok(checkpoint.clone())
    }

    pub fn checkpoint(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionCheckpoint>, CheckpointError> {
        let mut state = self.state.lock();
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CheckpointError::SessionNotFound)?;
        session.last_accessed = Instant::now();
        Ok(session.checkpoint.clone())
    }

    pub fn clear_checkpoint(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionCheckpoint>, CheckpointError> {
        let mut state = self.state.lock();
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CheckpointError::SessionNotFound)?;
        session.last_accessed = Instant::now();
        session.checkpoint = None;
        Ok(None)
    }

    pub fn data_inventory(&self) -> Vec<SessionDataSummary> {
        let state = self.state.lock();
        let mut sessions = state
            .sessions
            .values()
            .map(session_data_summary)
            .collect::<Vec<_>>();
        sessions.sort_unstable_by_key(|session| session.session_id);
        sessions
    }

    pub fn data_limits(&self) -> SessionDataLimits {
        SessionDataLimits {
            max_commands_per_session: self.limits.max_commands,
            max_output_chars_per_command: self.limits.max_output_per_command,
            max_total_context_chars_per_session: self.limits.max_total_chars,
            chat_history_enabled: self.limits.history_enabled,
            max_chat_messages_per_session: self.limits.max_chat_messages,
            max_sessions: self.limits.max_sessions,
            disconnected_session_ttl_seconds: self.limits.disconnected_session_ttl.as_secs(),
        }
    }

    pub fn clear_session_data(
        &self,
        session_id: SessionId,
        discard_in_flight: bool,
    ) -> Option<DataRemovalSummary> {
        let mut state = self.state.lock();
        let session = state.sessions.get_mut(&session_id)?;
        let removed = clear_transient_session_data(session, discard_in_flight);
        session.last_accessed = Instant::now();
        Some(removed)
    }

    pub fn clear_chat_history(&self) -> (Vec<SessionId>, DataRemovalSummary) {
        let mut state = self.state.lock();
        // Every open Coach window must receive the clear event, including a
        // window that loaded persisted history before this daemon retained any
        // chat for the session. Otherwise that window could write deleted
        // history back to disk when it exits.
        let mut affected = state.sessions.keys().copied().collect::<Vec<_>>();
        let mut removed = DataRemovalSummary::default();
        for session in state.sessions.values_mut() {
            let chat_requests = session
                .active
                .iter()
                .filter_map(|(request_id, request)| {
                    (request.kind == ActiveRequestKind::Chat)
                        .then_some((*request_id, request.cancellation.clone()))
                })
                .collect::<Vec<_>>();
            if !session.chat.is_empty() || !chat_requests.is_empty() {
                removed.sessions_affected += 1;
            }
            removed.chat_messages += session.chat.len();
            removed.active_ai_requests += chat_requests.len();
            session.chat.clear();
            for (request_id, cancellation) in chat_requests {
                cancellation.cancel();
                session.active.remove(&request_id);
            }
            session.last_accessed = Instant::now();
        }
        affected.sort_unstable();
        (affected, removed)
    }

    pub fn clear_all_transient(
        &self,
        discard_in_flight: bool,
    ) -> (Vec<SessionId>, DataRemovalSummary) {
        let mut state = self.state.lock();
        let mut affected = state.sessions.keys().copied().collect::<Vec<_>>();
        let mut removed = DataRemovalSummary::default();
        for session in state.sessions.values_mut() {
            accumulate_removed(
                &mut removed,
                clear_transient_session_data(session, discard_in_flight),
            );
            session.last_accessed = Instant::now();
        }
        affected.sort_unstable();
        (affected, removed)
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
            checkpoint: session.checkpoint.clone().map(Box::new),
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
        if kind == ActiveRequestKind::Completion
            && let Some((old_id, old)) = session
                .active
                .iter()
                .find(|(_, active)| active.kind == ActiveRequestKind::Completion)
                .map(|(id, active)| (*id, active.cancellation.clone()))
        {
            old.cancel();
            session.active.remove(&old_id);
            superseded = Some(old_id);
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

fn bounded_checkpoint_text(value: &str, multiline: bool, max_chars: usize) -> Option<String> {
    let safe = strip_terminal_sequences(value, multiline);
    let safe = safe.trim();
    if safe.is_empty() {
        return None;
    }
    Some(safe.chars().take(max_chars).collect())
}

fn session_data_summary(session: &Session) -> SessionDataSummary {
    SessionDataSummary {
        session_id: session.id,
        connected: session.connection_id.is_some(),
        command_records: session.commands.len(),
        chat_messages: session.chat.len(),
        environment_values: session.environment.len(),
        checkpoint_present: session.checkpoint.is_some(),
        environment_baseline_present: session.last_success.is_some(),
        in_flight_commands: session.started.len(),
        discarded_finish_markers: session.discarded_commands.len(),
        active_ai_requests: session.active.len(),
        pending_failure: false,
    }
}

fn clear_transient_session_data(
    session: &mut Session,
    discard_in_flight: bool,
) -> DataRemovalSummary {
    let removed = DataRemovalSummary {
        sessions_affected: 1,
        command_records: session.commands.len(),
        chat_messages: session.chat.len(),
        environment_values: session.environment.len(),
        checkpoints: usize::from(session.checkpoint.is_some()),
        environment_baselines: usize::from(session.last_success.is_some()),
        in_flight_commands: session.started.len(),
        active_ai_requests: session.active.len(),
        ..DataRemovalSummary::default()
    };
    if discard_in_flight {
        session
            .discarded_commands
            .extend(session.started.keys().copied());
    }
    for request in session.active.values() {
        request.cancellation.cancel();
    }
    session.started.clear();
    session.commands.clear();
    session.chat.clear();
    session.active.clear();
    session.environment.clear();
    session.cwd = PathBuf::new();
    session.checkpoint = None;
    session.last_success = None;
    removed
}

fn accumulate_removed(total: &mut DataRemovalSummary, removed: DataRemovalSummary) {
    total.sessions_affected += removed.sessions_affected;
    total.command_records += removed.command_records;
    total.chat_messages += removed.chat_messages;
    total.persisted_chat_messages += removed.persisted_chat_messages;
    total.failure_fingerprints += removed.failure_fingerprints;
    total.environment_values += removed.environment_values;
    total.checkpoints += removed.checkpoints;
    total.environment_baselines += removed.environment_baselines;
    total.in_flight_commands += removed.in_flight_commands;
    total.active_ai_requests += removed.active_ai_requests;
    total.pending_failures += removed.pending_failures;
    total.source_card_cache_entries += removed.source_card_cache_entries;
}

fn sole_started_command_id(session: &Session) -> Option<CommandId> {
    (session.started.len() == 1)
        .then(|| session.started.keys().next().copied())
        .flatten()
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
            .unwrap()
            .into_recorded()
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

    #[test]
    fn failed_command_receives_the_latest_successful_environment_baseline() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        let mut params = registration(session);
        params.environment = BTreeMap::from([
            ("VIRTUAL_ENV".to_owned(), "/work/old/.venv".to_owned()),
            ("API_TOKEN".to_owned(), "must-not-be-retained".to_owned()),
        ]);
        manager.register(ConnectionId::new(), params);

        let successful = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: successful,
                command: "cargo check".to_owned(),
                cwd: PathBuf::from("/work/old"),
                started_at_unix_ms: None,
            },
        ));
        manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id: successful,
                    command: None,
                    cwd: None,
                    exit_code: 0,
                    stdout: None,
                    stderr: None,
                    duration_ms: Some(10),
                    environment: BTreeMap::from([(
                        "VIRTUAL_ENV".to_owned(),
                        "/work/old/.venv".to_owned(),
                    )]),
                },
                None,
            )
            .unwrap()
            .into_recorded()
            .unwrap();

        let failed = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: failed,
                command: "cargo test".to_owned(),
                cwd: PathBuf::from("/work/new"),
                started_at_unix_ms: None,
            },
        ));
        let job = manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id: failed,
                    command: None,
                    cwd: None,
                    exit_code: 1,
                    stdout: None,
                    stderr: Some("failed".to_owned()),
                    duration_ms: Some(20),
                    environment: BTreeMap::from([
                        ("CONDA_DEFAULT_ENV".to_owned(), "ml".to_owned()),
                        ("AWS_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned()),
                    ]),
                },
                None,
            )
            .unwrap()
            .into_recorded()
            .unwrap();

        let baseline = job.last_success.expect("successful baseline");
        assert_eq!(baseline.command_id, successful);
        assert_eq!(baseline.snapshot.cwd, PathBuf::from("/work/old"));
        assert_eq!(
            baseline.snapshot.environment,
            BTreeMap::from([("VIRTUAL_ENV".to_owned(), "/work/old/.venv".to_owned())])
        );
        assert!(!baseline.snapshot.environment.contains_key("API_TOKEN"));
        assert!(
            !baseline
                .snapshot
                .environment
                .contains_key("AWS_SECRET_ACCESS_KEY")
        );
    }

    #[test]
    fn stale_git_probe_cannot_overwrite_a_newer_successful_baseline() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        manager.register(ConnectionId::new(), registration(session));

        let first = CommandId::new();
        let second = CommandId::new();
        for (command_id, command) in [(first, "true"), (second, "pwd")] {
            assert!(manager.start_command(
                session,
                CommandStartedParams {
                    command_id,
                    command: command.to_owned(),
                    cwd: PathBuf::from("/work/repo"),
                    started_at_unix_ms: None,
                },
            ));
            manager
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
                        environment: BTreeMap::new(),
                    },
                    None,
                )
                .unwrap();
        }

        let stale_git = GitContext {
            repo_root: PathBuf::from("/work/stale"),
            branch: Some("stale".to_owned()),
            ..GitContext::default()
        };
        let current_git = GitContext {
            repo_root: PathBuf::from("/work/repo"),
            branch: Some("main".to_owned()),
            ..GitContext::default()
        };
        assert!(!manager.record_success_git(session, first, true, Some(stale_git)));
        assert!(manager.record_success_git(session, second, true, Some(current_git.clone())));

        let failed = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: failed,
                command: "false".to_owned(),
                cwd: PathBuf::from("/work/repo"),
                started_at_unix_ms: None,
            },
        ));
        let job = manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id: failed,
                    command: None,
                    cwd: None,
                    exit_code: 1,
                    stdout: None,
                    stderr: Some("failed".to_owned()),
                    duration_ms: Some(1),
                    environment: BTreeMap::new(),
                },
                None,
            )
            .unwrap()
            .into_recorded()
            .unwrap();
        let baseline = job.last_success.expect("successful baseline");
        assert_eq!(baseline.command_id, second);
        assert!(baseline.snapshot.git_observed);
        assert_eq!(baseline.snapshot.git, Some(current_git));
    }

    #[test]
    fn checkpoint_tracks_a_bounded_terminal_safe_resolution_after_its_marker() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        manager.register(ConnectionId::new(), registration(session));
        assert_eq!(
            manager.resolve_checkpoint(session, "fixed", 20, false),
            Err(CheckpointError::NoActiveCheckpoint)
        );

        let command_id = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id,
                command: "cargo test".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        ));
        manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id,
                    command: None,
                    cwd: None,
                    exit_code: 1,
                    stdout: None,
                    stderr: Some("failed".to_owned()),
                    duration_ms: Some(1),
                    environment: BTreeMap::new(),
                },
                None,
            )
            .unwrap();

        let checkpoint = manager
            .start_checkpoint(session, "  Build \u{1b}[31mregression\n  ", 10, false)
            .unwrap();
        assert_eq!(checkpoint.name, "Build regression");
        assert_eq!(checkpoint.started_after_command_id, Some(command_id));
        let resolution = format!(
            "first\nsecond\u{1b}]52;c;payload\u{7}\n{}",
            "x".repeat(2_100)
        );
        let resolved = manager
            .resolve_checkpoint(session, &resolution, 20, false)
            .unwrap();
        let resolution = resolved.resolution.as_deref().unwrap();
        assert!(resolution.starts_with("first\nsecond\n"));
        assert!(!resolution.contains('\u{1b}'));
        assert!(!resolution.contains("payload"));
        assert_eq!(resolution.chars().count(), MAX_CHECKPOINT_RESOLUTION_CHARS);
        assert_eq!(resolved.resolved_at_unix_ms, Some(20));
        assert_eq!(resolved.resolved_after_command_id, Some(command_id));
        assert_eq!(
            manager
                .context(session, None)
                .unwrap()
                .checkpoint
                .map(|checkpoint| *checkpoint),
            Some(resolved)
        );
        assert_eq!(manager.clear_checkpoint(session).unwrap(), None);
        assert_eq!(manager.checkpoint(session).unwrap(), None);
    }

    #[test]
    fn checkpoint_marks_its_own_inflight_cli_commands_for_capsule_exclusion() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        manager.register(ConnectionId::new(), registration(session));

        let start_cli = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: start_cli,
                command: "aicoach checkpoint start issue".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        ));
        let checkpoint = manager
            .start_checkpoint(session, "issue", 10, true)
            .unwrap();
        assert_eq!(checkpoint.start_command_id, Some(start_cli));
        assert!(checkpoint.started_after_command_id.is_none());
        manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id: start_cli,
                    command: None,
                    cwd: None,
                    exit_code: 0,
                    stdout: None,
                    stderr: None,
                    duration_ms: Some(1),
                    environment: BTreeMap::new(),
                },
                None,
            )
            .unwrap();

        let resolve_cli = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: resolve_cli,
                command: "aicoach checkpoint resolve".to_owned(),
                cwd: PathBuf::from("/tmp/a"),
                started_at_unix_ms: None,
            },
        ));
        let checkpoint = manager
            .resolve_checkpoint(session, "fixed", 20, true)
            .unwrap();
        assert_eq!(checkpoint.resolved_after_command_id, Some(start_cli));
        assert_eq!(checkpoint.resolution_command_id, Some(resolve_cli));
    }

    #[test]
    fn session_data_clear_cancels_and_discards_every_transient_category() {
        let manager = SessionManager::new(SessionLimits::default());
        let session = SessionId::new();
        let mut params = registration(session);
        params.environment = BTreeMap::from([("LANG".to_owned(), "en_US.UTF-8".to_owned())]);
        manager.register(ConnectionId::new(), params);

        let completed = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: completed,
                command: "cargo check".to_owned(),
                cwd: PathBuf::from("/work/private"),
                started_at_unix_ms: None,
            },
        ));
        manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id: completed,
                    command: None,
                    cwd: None,
                    exit_code: 0,
                    stdout: Some("private output".to_owned()),
                    stderr: None,
                    duration_ms: Some(1),
                    environment: BTreeMap::new(),
                },
                None,
            )
            .unwrap()
            .into_recorded()
            .unwrap();
        manager
            .start_checkpoint(session, "private checkpoint", 10, false)
            .unwrap();
        assert!(manager.push_chat(session, true, "private chat".to_owned()));
        let (cancellation, _) = manager
            .begin_request(session, RequestId::new(), ActiveRequestKind::Chat)
            .unwrap();
        let clearing = CommandId::new();
        assert!(manager.start_command(
            session,
            CommandStartedParams {
                command_id: clearing,
                command: "aicoach data clear session".to_owned(),
                cwd: PathBuf::from("/work/private"),
                started_at_unix_ms: None,
            },
        ));

        let before = manager.data_inventory().pop().unwrap();
        assert_eq!(before.command_records, 1);
        assert_eq!(before.chat_messages, 1);
        assert_eq!(before.environment_values, 1);
        assert!(before.checkpoint_present);
        assert!(before.environment_baseline_present);
        assert_eq!(before.in_flight_commands, 1);
        assert_eq!(before.active_ai_requests, 1);

        let removed = manager.clear_session_data(session, true).unwrap();
        assert_eq!(removed.sessions_affected, 1);
        assert_eq!(removed.command_records, 1);
        assert_eq!(removed.chat_messages, 1);
        assert_eq!(removed.environment_values, 1);
        assert_eq!(removed.checkpoints, 1);
        assert_eq!(removed.environment_baselines, 1);
        assert_eq!(removed.in_flight_commands, 1);
        assert_eq!(removed.active_ai_requests, 1);
        assert!(cancellation.is_cancelled());

        let finish = manager
            .finish_command(
                session,
                RequestId::new(),
                CommandFinishedParams {
                    command_id: clearing,
                    command: None,
                    cwd: None,
                    exit_code: 0,
                    stdout: None,
                    stderr: None,
                    duration_ms: Some(1),
                    environment: BTreeMap::new(),
                },
                None,
            )
            .unwrap();
        assert!(matches!(finish, FinishCommand::Discarded));
        let context = manager.context(session, None).unwrap();
        assert!(context.cwd.as_os_str().is_empty());
        assert!(context.environment.is_empty());
        assert!(context.commands.is_empty());
        assert!(context.checkpoint.is_none());

        let after = manager.data_inventory().pop().unwrap();
        assert!(after.connected);
        assert_eq!(after.command_records, 0);
        assert_eq!(after.chat_messages, 0);
        assert_eq!(after.environment_values, 0);
        assert!(!after.checkpoint_present);
        assert!(!after.environment_baseline_present);
        assert_eq!(after.in_flight_commands, 0);
        assert_eq!(after.active_ai_requests, 0);
    }

    #[test]
    fn chat_clear_notifies_sessions_with_only_persisted_window_history() {
        let manager = SessionManager::new(SessionLimits::default());
        let memory_chat = SessionId::new();
        let disk_only_chat = SessionId::new();
        manager.register(ConnectionId::new(), registration(memory_chat));
        manager.register(ConnectionId::new(), registration(disk_only_chat));
        assert!(manager.push_chat(memory_chat, true, "private chat".to_owned()));

        let (affected, removed) = manager.clear_chat_history();

        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&memory_chat));
        assert!(affected.contains(&disk_only_chat));
        assert_eq!(removed.sessions_affected, 1);
        assert_eq!(removed.chat_messages, 1);
    }
}
