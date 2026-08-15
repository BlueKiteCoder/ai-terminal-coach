function run() {
  const systemEvents = Application("System Events");
  for (const processName of ["Terminal", "iTerm2"]) {
    try {
      const process = systemEvents.applicationProcesses.byName(processName);
      for (const window of process.windows()) {
        if (String(window.name()).includes("AI Terminal Coach")) {
          window.attributes.byName("AXMinimized").value = true;
        }
      }
    } catch (_) {}
  }
}
