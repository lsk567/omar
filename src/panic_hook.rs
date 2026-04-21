//! Persisted panic logging.
//!
//! When omar runs as a child of a tmux session, stderr is attached to a
//! tmux pane. If the tmux server dies (see issue #118) it takes the pane
//! with it and any Rust panic message printed there is lost. This module
//! installs a panic hook that writes panic details to
//! `~/.omar/logs/panics/<ts>-pid<N>.log` **before** chaining to the
//! previous hook, so the default stderr backtrace still happens for
//! normal runs and we have a durable on-disk record for post-mortems.
//!
//! Install this as the very first thing in `main()`, before the tokio
//! runtime starts, so panics on any worker thread are also persisted.
//!
//! IO failures in the hook are swallowed (with a best-effort `eprintln!`)
//! because panicking inside a panic hook aborts the process.

use std::backtrace::Backtrace;
use std::fs::{self, File};
use std::io::Write;
use std::panic;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Install a panic hook that persists panic details under `log_dir`.
///
/// The previous hook (captured via `panic::take_hook()`) is invoked
/// *after* writing the file so existing stderr / `RUST_BACKTRACE`
/// behaviour is preserved. We do not call `process::abort()` — unwind
/// proceeds normally.
pub fn install(log_dir: PathBuf) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Best-effort: build the log entry and write it. Any IO error
        // must not cause a second panic inside the hook.
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed").to_string();
        let payload = payload_as_str(info).to_string();
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let backtrace = Backtrace::force_capture();

        let entry = format_panic_log_entry(&thread_name, &payload, &location, &backtrace);

        if let Err(e) = write_entry(&log_dir, &entry) {
            // Last-ditch: print to stderr. If stderr is gone (tmux
            // pane died) this is a no-op, but at least we tried.
            let _ = writeln!(std::io::stderr(), "[omar] panic-hook write failed: {e}");
        }

        // Chain to the previous hook so default stderr / RUST_BACKTRACE
        // behaviour still fires.
        previous(info);
    }));
}

/// Extract the panic payload as a string.
///
/// `PanicHookInfo::payload()` is `&dyn Any`; rustc downcasts the common
/// cases to `&str` or `String`. Anything else is reported as a
/// placeholder so the log entry stays useful.
fn payload_as_str<'a>(info: &'a panic::PanicHookInfo<'_>) -> &'a str {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
    }
}

/// Render a panic log entry. Pure — easy to unit test.
pub fn format_panic_log_entry(
    thread_name: &str,
    payload: &str,
    location: &str,
    backtrace: &Backtrace,
) -> String {
    format!(
        "thread '{thread}' panicked at {location}:\n\
         {payload}\n\
         \n\
         stack backtrace:\n\
         {backtrace}\n",
        thread = thread_name,
        location = location,
        payload = payload,
        backtrace = backtrace,
    )
}

/// Write `entry` to `<log_dir>/<ts>-pid<N>.log`.
fn write_entry(log_dir: &Path, entry: &str) -> std::io::Result<()> {
    fs::create_dir_all(log_dir)?;
    let filename = panic_filename(SystemTime::now(), std::process::id());
    let path = log_dir.join(filename);
    let mut file = File::create(&path)?;
    file.write_all(entry.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Build a filesystem-safe filename of the form
/// `<unix_nanos>-pid<N>.log`. Using unix nanoseconds keeps names
/// monotonic-ish and trivially sortable; including the pid prevents
/// collisions when two processes panic in the same nanosecond.
fn panic_filename(now: SystemTime, pid: u32) -> String {
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-pid{pid}.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Serialise tests that swap the process-wide panic hook so they
    /// don't race each other.
    fn hook_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn format_contains_all_fields() {
        let bt = Backtrace::capture(); // empty unless RUST_BACKTRACE=1
        let out = format_panic_log_entry("worker-3", "boom!", "src/foo.rs:10:5", &bt);
        assert!(out.contains("worker-3"), "thread name missing: {out}");
        assert!(out.contains("boom!"), "payload missing: {out}");
        assert!(out.contains("src/foo.rs:10:5"), "location missing: {out}");
        assert!(out.contains("stack backtrace:"), "header missing: {out}");
    }

    #[test]
    fn panic_filename_is_safe_and_contains_pid() {
        let name = panic_filename(SystemTime::now(), 4242);
        assert!(name.ends_with("-pid4242.log"), "bad suffix: {name}");
        // No path separators, colons, or spaces — filesystem-safe.
        for bad in [':', '/', '\\', ' '] {
            assert!(!name.contains(bad), "unsafe char {bad:?} in {name}");
        }
    }

    #[test]
    fn install_writes_file_on_panic() {
        let _guard = hook_lock().lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().to_path_buf();

        // Snapshot whatever hook was installed before this test so we
        // can restore it and not leak our hook into sibling tests.
        let previous_before = panic::take_hook();
        panic::set_hook(previous_before);

        install(log_dir.clone());

        // `catch_unwind` lets the hook fire without killing the test.
        let result = std::panic::catch_unwind(|| {
            panic!("persisted-panic-test");
        });
        assert!(result.is_err(), "expected panic");

        // Restore a no-op hook so this test doesn't affect later tests.
        panic::set_hook(Box::new(|_| {}));

        let mut entries: Vec<_> = fs::read_dir(&log_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "expected exactly one log file");

        let path = entries.pop().unwrap().path();
        let filename = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            filename.contains(&format!("-pid{}.log", std::process::id())),
            "unexpected filename: {filename}"
        );

        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("persisted-panic-test"),
            "payload missing in file: {contents}"
        );
        assert!(
            contents.contains("stack backtrace:"),
            "backtrace header missing in file: {contents}"
        );
    }
}
