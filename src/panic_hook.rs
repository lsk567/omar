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
//! runtime is built, so panics on any worker thread are also
//! persisted. Note that `#[tokio::main]` constructs the runtime
//! *before* the `async fn main()` body runs, so callers that need the
//! earliest-possible coverage should use a synchronous `fn main()`
//! that installs the hook and then builds the runtime by hand.
//!
//! IO failures in the hook are swallowed (with a best-effort `eprintln!`)
//! because panicking inside a panic hook aborts the process.

use std::backtrace::Backtrace;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of filename-collision retries in `write_entry`.
/// `SystemTime::now()` has coarse resolution on some platforms; if two
/// panics land in the same nanosecond bucket within the same pid we
/// append an attempt counter instead of truncating an earlier log.
const MAX_COLLISION_RETRIES: u32 = 16;

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

/// Write `entry` to `<log_dir>/<ts>-pid<N>.log`. Uses
/// `OpenOptions::create_new` so an unexpected existing file is never
/// truncated; on collision (coarse clock + same pid) we append an
/// attempt counter and retry.
fn write_entry(log_dir: &Path, entry: &str) -> io::Result<()> {
    fs::create_dir_all(log_dir)?;
    let pid = std::process::id();

    for attempt in 0..MAX_COLLISION_RETRIES {
        let filename = panic_filename(SystemTime::now(), pid, attempt);
        let path = log_dir.join(&filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(entry.as_bytes())?;
                file.flush()?;
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "panic log filename collisions exceeded retry budget",
    ))
}

/// Build a filesystem-safe filename. Base form is
/// `<unix_nanos>-pid<N>.log`; on collision retries the form becomes
/// `<unix_nanos>-pid<N>-<attempt>.log`. Unix-nanosecond prefix keeps
/// names monotonic-ish and trivially sortable; pid prevents collisions
/// between concurrent processes.
fn panic_filename(now: SystemTime, pid: u32, attempt: u32) -> String {
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    if attempt == 0 {
        format!("{nanos}-pid{pid}.log")
    } else {
        format!("{nanos}-pid{pid}-{attempt}.log")
    }
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

    type BoxedHook = Box<dyn Fn(&panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

    /// RAII guard that captures the current panic hook on
    /// construction and restores it on drop — even if the test
    /// unwinds through an assertion. Prevents leaking a test hook
    /// into sibling tests.
    struct PanicHookGuard {
        previous: Option<BoxedHook>,
    }

    impl PanicHookGuard {
        fn new() -> Self {
            Self {
                previous: Some(panic::take_hook()),
            }
        }
    }

    impl Drop for PanicHookGuard {
        fn drop(&mut self) {
            if let Some(hook) = self.previous.take() {
                panic::set_hook(hook);
            }
        }
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
        let name = panic_filename(SystemTime::now(), 4242, 0);
        assert!(name.ends_with("-pid4242.log"), "bad suffix: {name}");
        // No path separators, colons, or spaces — filesystem-safe.
        for bad in [':', '/', '\\', ' '] {
            assert!(!name.contains(bad), "unsafe char {bad:?} in {name}");
        }
    }

    #[test]
    fn panic_filename_disambiguates_on_retry() {
        let a = panic_filename(SystemTime::now(), 4242, 0);
        let b = panic_filename(SystemTime::now(), 4242, 1);
        assert_ne!(a, b, "retry must produce a different filename");
        assert!(b.contains("-1.log"), "attempt suffix missing: {b}");
    }

    #[test]
    fn install_writes_file_on_panic() {
        let _serial = hook_lock().lock().unwrap();
        // Restore whatever hook was previously installed on drop —
        // even if an assertion below panics.
        let _restore = PanicHookGuard::new();

        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().to_path_buf();

        install(log_dir.clone());

        // `catch_unwind` lets the hook fire without killing the test.
        let result = std::panic::catch_unwind(|| {
            panic!("persisted-panic-test");
        });
        assert!(result.is_err(), "expected panic");

        let mut entries: Vec<_> = fs::read_dir(&log_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "expected exactly one log file");

        let path = entries.pop().unwrap().path();
        let filename = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            filename.contains(&format!("-pid{}", std::process::id())),
            "unexpected filename: {filename}"
        );
        assert!(filename.ends_with(".log"), "bad extension: {filename}");

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

    #[test]
    fn write_entry_does_not_truncate_existing_file() {
        // Even without a racing panic, verify `write_entry` creates a
        // second file via the collision-retry path when a log with the
        // primary name already exists.
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().to_path_buf();

        // First call writes one file.
        write_entry(&log_dir, "first").unwrap();
        let first_entries: Vec<_> = fs::read_dir(&log_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(first_entries.len(), 1);

        // Second call in the (extremely unlikely) same nanosecond would
        // collide on the primary filename. Simulate that by manually
        // creating a file at the exact primary name, then confirming
        // `write_entry` writes a disambiguated file instead of
        // overwriting.
        let now = SystemTime::now();
        let primary = log_dir.join(panic_filename(now, std::process::id(), 0));
        fs::write(&primary, b"DO-NOT-OVERWRITE").unwrap();

        write_entry(&log_dir, "second").unwrap();
        let after: Vec<_> = fs::read_dir(&log_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        // We expect 3+ files: the original first-call file, the
        // planted primary, and the retry-disambiguated second-call
        // file. Crucially, the planted file must still say
        // DO-NOT-OVERWRITE.
        assert!(after.len() >= 3, "expected retry file: {after:?}");
        assert_eq!(fs::read(&primary).unwrap(), b"DO-NOT-OVERWRITE");
    }
}
