use std::{
    fs,
    io::{self, Write as _},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeFileError {
    #[error("runtime path has no parent: {0}")]
    MissingParent(PathBuf),
    #[error("another daemon is running with pid {0}")]
    AlreadyRunning(u32),
    #[error("runtime file operation failed: {0}")]
    Io(#[from] io::Error),
}

pub struct RuntimeFiles {
    socket_path: PathBuf,
    pid_path: PathBuf,
}

/// Atomically records the shell that most recently registered or focused.
/// Both files are mode 0600 and contain one percent-unescaped value plus `\n`.
///
/// # Errors
///
/// Returns an error if private runtime markers cannot be created, synchronized,
/// or atomically replaced.
pub fn write_active_session(
    runtime_dir: &Path,
    session_id: &str,
    tty: &str,
) -> Result<(), RuntimeFileError> {
    fs::create_dir_all(runtime_dir)?;
    fs::set_permissions(runtime_dir, fs::Permissions::from_mode(0o700))?;
    atomic_private_write(&runtime_dir.join("active-session"), session_id.as_bytes())?;
    atomic_private_write(&runtime_dir.join("active-tty"), tty.as_bytes())?;
    Ok(())
}

impl RuntimeFiles {
    /// Reserves the PID and socket paths for this process.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeFileError::AlreadyRunning`] for a live daemon, or an
    /// I/O/path error if the private runtime files cannot be prepared.
    pub fn acquire(socket_path: PathBuf, pid_path: PathBuf) -> Result<Self, RuntimeFileError> {
        prepare_parent(&socket_path)?;
        prepare_parent(&pid_path)?;
        if let Some(pid) = read_pid(&pid_path)? {
            if process_is_alive(pid) {
                return Err(RuntimeFileError::AlreadyRunning(pid));
            }
            remove_if_exists(&pid_path)?;
        }

        if socket_path.exists() {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                return Err(RuntimeFileError::AlreadyRunning(0));
            }
            remove_if_exists(&socket_path)?;
        }

        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true).mode(0o600);
        let mut file = options.open(&pid_path)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        fs::set_permissions(&pid_path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            socket_path,
            pid_path,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn pid_path(&self) -> &Path {
        &self.pid_path
    }

    /// Restricts an already-bound socket to its owner.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if socket permissions cannot be changed.
    pub fn secure_socket(&self) -> Result<(), RuntimeFileError> {
        fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    pub fn cleanup(&self) {
        let _ = remove_if_exists(&self.socket_path);
        let _ = remove_if_exists(&self.pid_path);
    }
}

impl Drop for RuntimeFiles {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn prepare_parent(path: &Path) -> Result<(), RuntimeFileError> {
    let parent = path
        .parent()
        .ok_or_else(|| RuntimeFileError::MissingParent(path.to_owned()))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn read_pid(path: &Path) -> Result<Option<u32>, io::Error> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(value.trim().parse().ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    output.status.success()
        && executable_name(&output.stdout).is_some_and(|name| name == "aicoachd")
}

fn executable_name(command: &[u8]) -> Option<&str> {
    let command = std::str::from_utf8(command).ok()?.trim();
    Path::new(command).file_name()?.to_str()
}

fn remove_if_exists(path: &Path) -> Result<(), io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn atomic_private_write(path: &Path, value: &[u8]) -> Result<(), io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("active");
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true).mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_private_runtime_files_and_cleans_up() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run/coach.sock");
        let pid = directory.path().join("run/coach.pid");
        {
            let guard = RuntimeFiles::acquire(socket.clone(), pid.clone()).unwrap();
            assert!(guard.pid_path().exists());
            let mode = fs::metadata(&pid).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(!socket.exists());
        assert!(!pid.exists());
    }

    #[test]
    fn active_session_files_are_private_and_replaced() {
        let directory = tempfile::tempdir().unwrap();
        write_active_session(directory.path(), "first", "/dev/ttys001").unwrap();
        write_active_session(directory.path(), "second", "/dev/ttys002").unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("active-session")).unwrap(),
            "second\n"
        );
        assert_eq!(
            fs::metadata(directory.path().join("active-tty"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn daemon_identity_uses_the_executable_name() {
        assert_eq!(
            executable_name(b"/opt/aicoach/bin/aicoachd\n"),
            Some("aicoachd")
        );
        assert_eq!(executable_name(b"/bin/sleep\n"), Some("sleep"));
        assert_eq!(executable_name(&[0xff]), None);
    }
}
