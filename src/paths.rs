//! User-private temp files for payloads handed off to tmux / backend CLIs.
//! Files live in a per-user dir (0700) and are created 0600 with `create_new`,
//! so other accounts on a shared host can't read the payloads.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// A 0600 temp file that deletes itself on drop (even on panic / early return).
pub struct PrivateTempFile {
    path: PathBuf,
    file: File,
}

impl PrivateTempFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Write for PrivateTempFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for PrivateTempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Per-user dir (mode 0700): `$XDG_RUNTIME_DIR` if set, else `temp_dir()`,
/// with an `omar-<user>` subdir so the `/tmp` fallback isn't shared.
pub fn private_temp_dir() -> io::Result<PathBuf> {
    let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
    let user = sanitize_user(
        &std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_default(),
    );
    let dir = base.join(format!("omar-{user}"));
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// `$USER`/`$LOGNAME` are user-controlled, so strip everything but a safe
/// charset to keep the joined name from escaping `base` via separators or `..`.
fn sanitize_user(raw: &str) -> String {
    let s: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if s.is_empty() {
        "user".to_string()
    } else {
        s
    }
}

/// Self-deleting 0600 temp file under [`private_temp_dir`], named
/// `<prefix>-<uuid>.<ext>`.
pub fn create_private_temp_file(prefix: &str, ext: &str) -> io::Result<PrivateTempFile> {
    let dir = private_temp_dir()?;
    let path = dir.join(format!("{prefix}-{}.{ext}", Uuid::new_v4()));
    let file = create_private_file(&path)?;
    Ok(PrivateTempFile { path, file })
}

/// New 0600 file at `path` (no self-delete), for files that outlive the scope.
/// `create_new` fails rather than truncate or follow a planted symlink.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Ensure `dir` exists at mode 0700. An existing dir is tightened back to 0700,
/// which also errors if we don't own it.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::{DirBuilder, Permissions};
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        match DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                // Reject a pre-planted symlink or file before chmod follows it.
                if !std::fs::symlink_metadata(dir)?.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "private temp dir is not a directory",
                    ));
                }
                std::fs::set_permissions(dir, Permissions::from_mode(0o700))
            }
            Err(err) => Err(err),
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_temp_dir_is_owner_only() {
        let dir = private_temp_dir().expect("private temp dir");
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "private temp dir must be owner-only");
        }
    }

    #[test]
    fn create_private_temp_file_is_mode_0600() {
        let mut tmp = create_private_temp_file("omar-test", "txt").expect("temp file");
        tmp.write_all(b"secret payload").expect("write");
        assert!(tmp.path().exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "private temp file must be owner-only");
        }
    }

    #[test]
    fn create_private_temp_file_deletes_on_drop() {
        let path = {
            let tmp = create_private_temp_file("omar-test-drop", "txt").expect("temp file");
            tmp.path().to_path_buf()
        };
        assert!(!path.exists(), "file must be removed when guard is dropped");
    }

    #[test]
    fn create_private_file_refuses_existing_path() {
        let mut tmp = create_private_temp_file("omar-test-excl", "txt").expect("temp file");
        tmp.write_all(b"original").expect("write");
        // create_new must fail rather than truncate an existing file.
        let err = create_private_file(tmp.path()).expect_err("must not clobber existing file");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn sanitize_user_strips_path_separators() {
        assert_eq!(sanitize_user("alice"), "alice");
        assert_eq!(sanitize_user("../../etc"), "etc");
        assert_eq!(sanitize_user("a/b"), "ab");
        assert_eq!(sanitize_user(""), "user");
        assert_eq!(sanitize_user("///"), "user");
    }

    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_rejects_symlink() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("omar-symtest-{}", Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let target = base.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = base.join("omar-link");
        symlink(&target, &link).unwrap();
        let err = ensure_private_dir(&link).expect_err("symlink must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&base).ok();
    }
}
