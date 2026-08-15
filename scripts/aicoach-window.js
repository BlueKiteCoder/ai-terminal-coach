ObjC.import("Foundation");
ObjC.import("stdlib");

const STATE_PATH = ObjC.unwrap($.NSHomeDirectory()) + "/.aicoach/window-state.json";

function loadState() {
  try {
    const contents = $.NSString.stringWithContentsOfFileEncodingError(
      $(STATE_PATH), $.NSUTF8StringEncoding, null
    );
    return JSON.parse(ObjC.unwrap(contents));
  } catch (_) {
    return {};
  }
}

function saveState(changes) {
  try {
    const state = Object.assign(loadState(), changes);
    $(JSON.stringify(state)).writeToFileAtomicallyEncodingError(
      $(STATE_PATH), true, $.NSUTF8StringEncoding, null
    );
  } catch (_) {}
}

function validBounds(value) {
  return Array.isArray(value) && value.length === 4 && value.every(Number.isFinite) &&
    value[2] > value[0] + 200 && value[3] > value[1] + 150;
}

function configuredBounds(x, y, columns, rows) {
  const width = Math.max(480, columns * 8 + 40);
  const height = Math.max(300, rows * 18 + 80);
  return [x, y, x + width, y + height];
}

function integerArgument(value, fallback) {
  const parsed = Number.parseInt(String(value), 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function currentBounds(fallback) {
  const saved = loadState().bounds;
  return validBounds(saved) ? saved : fallback;
}

function frontmostApplication() {
  try {
    const processes = Application("System Events").applicationProcesses.whose({ frontmost: true });
    return processes.length > 0 ? processes[0].name() : "";
  } catch (_) {
    return "";
  }
}

function shellQuote(value) {
  return "'" + String(value).replace(/'/g, "'\\''") + "'";
}

function terminalWindowName(window) {
  try { return String(window.name()); } catch (_) { return ""; }
}

function coachAccessibilityWindow(processName) {
  try {
    const process = Application("System Events").applicationProcesses.byName(processName);
    for (const window of process.windows()) {
      if (String(window.name()).includes("AI Terminal Coach")) return window;
    }
  } catch (_) {}
  return null;
}

function saveAccessibleBounds(processName) {
  try {
    const window = coachAccessibilityWindow(processName);
    if (!window) return;
    const position = window.position();
    const size = window.size();
    const bounds = [position[0], position[1], position[0] + size[0], position[1] + size[1]];
    if (validBounds(bounds)) saveState({ bounds });
  } catch (_) {}
}

function restoreAccessibleBounds(processName, fallback) {
  try {
    const window = coachAccessibilityWindow(processName);
    if (!window) return;
    const bounds = currentBounds(fallback);
    window.position = [bounds[0], bounds[1]];
    window.size = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
  } catch (_) {}
}

function setCoachMinimized(processName, minimized) {
  try {
    const window = coachAccessibilityWindow(processName);
    if (window) window.attributes.byName("AXMinimized").value = minimized;
  } catch (_) {}
}

function toggleTerminal(command, fallback) {
  const terminal = Application("Terminal");
  terminal.activate();
  const windows = terminal.windows();
  if (windows.length > 0 && terminalWindowName(windows[0]).includes("AI Terminal Coach")) {
    try {
      const bounds = windows[0].bounds();
      if (validBounds(bounds)) saveState({ bounds });
    } catch (_) { saveAccessibleBounds("Terminal"); }
    windows[0].miniaturized = true;
    for (let index = 1; index < windows.length; index += 1) {
      if (!terminalWindowName(windows[index]).includes("AI Terminal Coach")) {
        windows[index].index = 1;
        break;
      }
    }
    return;
  }
  for (const window of windows) {
    if (terminalWindowName(window).includes("AI Terminal Coach")) {
      window.miniaturized = false;
      try { window.bounds = currentBounds(fallback); } catch (_) {}
      window.index = 1;
      return;
    }
  }
  terminal.doScript(command);
  delay(0.15);
  if (terminal.windows.length > 0) terminal.windows[0].bounds = currentBounds(fallback);
}

function sessionName(session) {
  try { return String(session.name()); } catch (_) { return ""; }
}

function allItermSessions(iterm) {
  const sessions = [];
  for (const window of iterm.windows()) {
    for (const tab of window.tabs()) {
      for (const session of tab.sessions()) sessions.push(session);
    }
  }
  return sessions;
}

function rememberOriginalItermSession(iterm) {
  try {
    const current = iterm.currentWindow.currentTab.currentSession;
    if (!sessionName(current).includes("AI Terminal Coach")) {
      saveState({ originalItermSessionId: String(current.id()) });
    }
  } catch (_) {}
}

function restoreOriginalItermSession(iterm) {
  const originalId = String(loadState().originalItermSessionId || "");
  if (!originalId) return;
  for (const session of allItermSessions(iterm)) {
    try {
      if (String(session.id()) === originalId) {
        session.select();
        return;
      }
    } catch (_) {}
  }
}

function toggleIterm(command, fallback) {
  const iterm = Application("iTerm2");
  iterm.activate();
  try {
    const current = iterm.currentWindow.currentTab.currentSession;
    if (sessionName(current).includes("AI Terminal Coach")) {
      saveAccessibleBounds("iTerm2");
      setCoachMinimized("iTerm2", true);
      restoreOriginalItermSession(iterm);
      return;
    }
  } catch (_) {}
  rememberOriginalItermSession(iterm);
  for (const session of allItermSessions(iterm)) {
    if (sessionName(session).includes("AI Terminal Coach")) {
      session.select();
      delay(0.1);
      setCoachMinimized("iTerm2", false);
      restoreAccessibleBounds("iTerm2", fallback);
      return;
    }
  }
  const window = iterm.createWindowWithDefaultProfileCommand(command);
  try { window.currentTab.currentSession.name = "AI Terminal Coach"; } catch (_) {}
  delay(0.15);
  restoreAccessibleBounds("iTerm2", fallback);
}

function run(argv) {
  const requestedSession = argv.length > 0 ? argv[0] : "";
  const columns = Math.max(40, integerArgument(argv[1], 100));
  const rows = Math.max(10, integerArgument(argv[2], 32));
  const x = integerArgument(argv[3], 120);
  const y = integerArgument(argv[4], 90);
  const preferred = String(argv[5] || "auto").toLowerCase();
  const fallback = configuredBounds(x, y, columns, rows);
  const command = "printf '\\033]0;AI Terminal Coach\\007\\033[8;" + rows + ";" + columns + "t'; exec aicoach-ui --managed-window --session " + shellQuote(requestedSession);
  const front = frontmostApplication();
  if (preferred === "iterm" || preferred === "iterm2" || preferred === "iterm.app") {
    return toggleIterm(command, fallback);
  }
  if (preferred === "terminal" || preferred === "terminal.app") {
    return toggleTerminal(command, fallback);
  }
  if (front === "iTerm2") return toggleIterm(command, fallback);
  if (front === "Terminal") return toggleTerminal(command, fallback);

  const iterm = Application("iTerm2");
  try {
    if (iterm.running()) return toggleIterm(command, fallback);
  } catch (_) {}
  return toggleTerminal(command, fallback);
}
