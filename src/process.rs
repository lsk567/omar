use std::fs;
use std::path::Path;

/// Return true if a process with the given PID currently exists. Uses
/// `kill -0 <pid>`, the standard POSIX no-op signal check. On non-Unix
/// platforms, conservatively assume the process is still alive rather than
/// reclaiming a lock we shouldn't.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return false;
        }
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

pub(crate) fn pid_file_is_stale(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .is_some_and(|pid| !pid_alive(pid))
}
