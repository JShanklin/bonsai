//! One bonsai command changes a tree at a time. Every command that writes to
//! a tree (bonsai.toml, branch and edge files, generated code, Cargo.toml,
//! .cargo/config.toml) holds the tree's lock, an exclusive `flock` on
//! `.bonsai.lock` in its folder, from before it reads what it'll change until
//! it's done; `bonsai sync`, `bonsai doctor` and `bonsai dev` read under a
//! shared one, so they never see a change half made. A command that finds the
//! tree locked waits `WAIT` (`BONSAI_LOCK_WAIT` seconds), saying once who
//! holds it, then gives up having changed nothing (`LockError::Busy`);
//! `bonsai dev` only tries, and keeps trying until it's free. A lock that
//! can't be taken at all is `LockError::Failed`: waiting won't help. The OS lets go of the lock
//! when its holder exits, however it exits, so a killed command never leaves
//! the tree locked.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

/// The lock file, in the tree's folder (git ignores it).
pub const LOCK_FILE: &str = ".bonsai.lock";
/// How long a command waits for another to finish, unless `BONSAI_LOCK_WAIT`
/// (seconds) says otherwise.
pub const WAIT: Duration = Duration::from_secs(10);

/// The tree's lock, held until dropped. Functions that change a tree take a
/// `&TreeLock`, so a command takes it once and hands it down.
#[derive(Debug)]
#[must_use = "the lock is let go as soon as it's dropped"]
pub struct TreeLock {
    _file: File,
}

/// Why the lock wasn't had.
#[derive(Debug, Clone, PartialEq)]
pub enum LockError {
    /// Another command holds the tree (`holder`: `\`bonsai link …\``, as it
    /// wrote it) and didn't let go in time. It will: trying again works.
    Busy { holder: String, message: String },
    /// The lock can't be taken at all (its file can't be opened or locked:
    /// permissions, a read-only folder). Waiting won't change that.
    Failed(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Busy { message, .. } | LockError::Failed(message) => f.write_str(message),
        }
    }
}

/// How long to wait for the lock: `BONSAI_LOCK_WAIT` seconds, else `WAIT`.
pub fn wait() -> Duration {
    std::env::var("BONSAI_LOCK_WAIT")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| *s >= 0.0)
        .map_or(WAIT, Duration::from_secs_f64)
}

fn flock(file: &File, how: i32) -> std::io::Result<()> {
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), how | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Who holds the lock, as its holder wrote it: `branch add (pid 4242)`.
fn holder(path: &Path) -> String {
    let mut text = String::new();
    let _ = File::open(path).and_then(|mut f| f.read_to_string(&mut text));
    let text = text.trim();
    if text.is_empty() {
        "another bonsai command".to_string()
    } else {
        format!("`bonsai {text}`")
    }
}

/// Take `file`'s lock (`how`: LOCK_EX or LOCK_SH), waiting up to `wait`;
/// `waiting` is told once if it has to wait.
fn take(
    file: &File,
    path: &Path,
    how: i32,
    wait: Duration,
    waiting: &mut dyn FnMut(&str),
) -> Result<(), LockError> {
    let started = Instant::now();
    let mut told = false;
    loop {
        match flock(file, how) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                return Err(LockError::Failed(format!(
                    "can't lock {}: {e}",
                    path.display()
                )));
            }
        }
        if started.elapsed() >= wait {
            let holder = holder(path);
            let message = format!(
                "{holder} is changing this tree; nothing was changed. Try again when it's done \
                 (waited {:.1}s; BONSAI_LOCK_WAIT sets how long)",
                wait.as_secs_f64()
            );
            return Err(LockError::Busy { holder, message });
        }
        if !told {
            told = true;
            waiting(&holder(path));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Take the tree's lock to change it, for `what` (a command, as `bonsai
/// <what>`), waiting up to `wait` for another command to finish.
pub fn acquire(root: &Path, what: &str, wait: Duration) -> Result<TreeLock, LockError> {
    acquire_telling(root, what, wait, &mut |who| {
        eprintln!("waiting for {who} to finish changing this tree…");
    })
}

/// `acquire`, telling `waiting` (instead of stderr) if it has to wait.
pub fn acquire_telling(
    root: &Path,
    what: &str,
    wait: Duration,
    waiting: &mut dyn FnMut(&str),
) -> Result<TreeLock, LockError> {
    let path = root.join(LOCK_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| LockError::Failed(format!("can't open {}: {e}", path.display())))?;
    take(&file, &path, libc::LOCK_EX, wait, waiting)?;
    // Say who holds it, for whoever has to wait (best effort).
    let _ = file.set_len(0);
    let _ = file.rewind();
    let _ = write!(file, "{what} (pid {})", std::process::id());
    let _ = file.flush();
    Ok(TreeLock { _file: file })
}

/// Run `read` on a consistent tree, writing nothing: under a shared lock
/// when the tree has a lock file (so no command changes it meanwhile), and
/// again if a command started changing it while `read` ran without one.
/// Err when no consistent reading could be had within `wait`.
pub fn read_consistent<T>(
    root: &Path,
    wait: Duration,
    mut read: impl FnMut() -> T,
) -> Result<T, LockError> {
    let path = root.join(LOCK_FILE);
    for _ in 0..3 {
        match File::open(&path) {
            Ok(file) => {
                take(&file, &path, libc::LOCK_SH, wait, &mut |_| {})?;
                return Ok(read());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let got = read();
                // No command had ever locked it; if one did meanwhile, read again.
                if !path.exists() {
                    return Ok(got);
                }
            }
            Err(e) => {
                return Err(LockError::Failed(format!(
                    "can't open {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Err(LockError::Busy {
        holder: "another bonsai command".to_string(),
        message: "the tree kept changing while it was read".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bonsai-lock-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn one_holder_at_a_time_and_waiters_give_up_in_time() {
        let root = dir("one");
        let held = acquire(&root, "branch add a", Duration::ZERO).unwrap();
        let started = Instant::now();
        let mut told = Vec::new();
        let err = acquire_telling(&root, "sync", Duration::from_millis(200), &mut |w| {
            told.push(w.to_string())
        })
        .unwrap_err()
        .to_string();
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(err.contains("`bonsai branch add a (pid "), "{err}");
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(told.len(), 1, "told once: {told:?}");
        // Readers wait for the writer too.
        assert!(read_consistent(&root, Duration::from_millis(50), || ()).is_err());
        drop(held);
        let _again = acquire(&root, "sync", Duration::ZERO).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn readers_share_and_hold_writers_off() {
        let root = dir("readers");
        drop(acquire(&root, "sync", Duration::ZERO).unwrap());
        let inner = read_consistent(&root, Duration::ZERO, || {
            // Another reader gets in; a writer doesn't.
            let nested = read_consistent(&root, Duration::ZERO, || 7).unwrap();
            let writer = acquire(&root, "link", Duration::from_millis(50));
            (nested, writer.is_err())
        })
        .unwrap();
        assert_eq!(inner, (7, true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_held_lock_is_busy_and_a_broken_one_failed() {
        let root = dir("kinds");
        let held = acquire(&root, "link a", Duration::ZERO).unwrap();
        match acquire(&root, "sync", Duration::ZERO).unwrap_err() {
            LockError::Busy { holder, .. } => assert!(holder.contains("link a"), "{holder}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            read_consistent(&root, Duration::ZERO, || ()),
            Err(LockError::Busy { .. })
        ));
        drop(held);
        // A lock file that can't be opened (a symlink to itself): no
        // waiting fixes that, for writers or readers.
        std::fs::remove_file(root.join(LOCK_FILE)).unwrap();
        std::os::unix::fs::symlink(LOCK_FILE, root.join(LOCK_FILE)).unwrap();
        assert!(matches!(
            acquire(&root, "sync", Duration::ZERO),
            Err(LockError::Failed(_))
        ));
        assert!(matches!(
            read_consistent(&root, Duration::ZERO, || ()),
            Err(LockError::Failed(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tree_never_locked_is_read_without_making_a_lock_file() {
        let root = dir("unlocked");
        assert_eq!(read_consistent(&root, Duration::ZERO, || 1).unwrap(), 1);
        assert!(!root.join(LOCK_FILE).exists(), "reading wrote a file");
        let _ = std::fs::remove_dir_all(&root);
    }
}
