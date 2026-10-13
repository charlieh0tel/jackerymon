//! Per-unit runtime files, named by serial:
//!
//! - The daemon's Unix socket.  Its permissions decide who may use the
//!   daemon, and its presence tells clients that a daemon owns the unit.
//! - A lock held by whichever process uses the unit's BLE connection, so
//!   no two jackerymon processes share or steal it.

use std::env;
use std::fs;
use std::fs::DirBuilder;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::TryLockError;
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use tokio::net::UnixListener;

/// Where a system daemon's socket lives; systemd's `RuntimeDirectory=`.
const SYSTEM_SOCKET_DIRECTORY: &str = "/run/jackerymon";
/// Under `$XDG_RUNTIME_DIR` for a daemon a user runs.
const USER_SOCKET_SUBDIRECTORY: &str = "jackerymon";
const SOCKET_EXTENSION: &str = "sock";
/// Shared by every user and the system daemon (sticky, world-writable),
/// so one lock file per unit serves them all.
const LOCK_DIRECTORY: &str = "/run/lock";
const LOCK_PREFIX: &str = "jackerymon-";
const LOCK_EXTENSION: &str = "lock";
/// Owner and group may connect.  A user's runtime directory is private;
/// the system daemon's admits the `jackerymon` group.
const SOCKET_MODE: u32 = 0o660;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

fn user_directory() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|directory| !directory.is_empty())
        .map(|directory| PathBuf::from(directory).join(USER_SOCKET_SUBDIRECTORY))
}

/// The socket for `serial` in `directory`.
fn socket_path(directory: &Path, serial: &str) -> PathBuf {
    directory.join(serial).with_extension(SOCKET_EXTENSION)
}

/// This process's runtime directory: systemd's when it sets one,
/// otherwise the user's.
fn own_directory() -> Result<PathBuf> {
    match env::var_os("RUNTIME_DIRECTORY") {
        Some(directory) if !directory.is_empty() => Ok(PathBuf::from(directory)),
        _ => user_directory().context("XDG_RUNTIME_DIR is not set"),
    }
}

fn create_private_directory(directory: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIRECTORY_MODE)
        .create(directory)
        .with_context(|| format!("creating {}", directory.display()))
}

/// Where a daemon for `serial` listens.
pub(crate) fn daemon_path(serial: &str) -> Result<PathBuf> {
    Ok(socket_path(&own_directory()?, serial))
}

/// This process's hold on one unit's BLE connection, released on drop.
pub(crate) struct UnitLock {
    _file: File,
}

/// Takes the unit for this process.  Fails if another jackerymon, of any
/// user or the system daemon, already holds it.
pub(crate) fn lock_unit(serial: &str) -> Result<UnitLock> {
    lock_in(Path::new(LOCK_DIRECTORY), serial)
}

fn lock_in(directory: &Path, serial: &str) -> Result<UnitLock> {
    let path = directory
        .join(format!("{LOCK_PREFIX}{serial}"))
        .with_extension(LOCK_EXTENSION);
    // A symlink planted in the shared directory is never followed.
    let created = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path);
    // Another user's lock cannot be opened for writing, or created over in
    // a sticky directory; reading it is enough to lock it.
    let file = match created {
        Err(error) if error.kind() == ErrorKind::PermissionDenied => OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path),
        other => other,
    }
    .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(UnitLock { _file: file }),
        Err(TryLockError::WouldBlock) => bail!(
            "unit {serial} is in use by another jackerymon ({}); use its daemon or stop it",
            path.display()
        ),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// A bound socket, removed when dropped.
pub(crate) struct Claimed {
    path: PathBuf,
    /// Device and inode of the socket bound, so a later daemon's socket
    /// at the same path is left alone.
    identity: (u64, u64),
}

impl Drop for Claimed {
    fn drop(&mut self) {
        if identity(&self.path).is_some_and(|current| current == self.identity) {
            fs::remove_file(&self.path).ok();
        }
    }
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

/// Binds `path` for this daemon.  Fails if a live daemon answers there;
/// removes a socket a dead one left behind.
pub(crate) fn claim(path: &Path) -> Result<(UnixListener, Claimed)> {
    match UnixStream::connect(path) {
        Ok(_) => bail!("a daemon already answers at {}", path.display()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => fs::remove_file(path)
            .with_context(|| format!("removing the stale socket {}", path.display()))?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("checking {}", path.display()));
        }
    }
    if let Some(directory) = path.parent() {
        create_private_directory(directory)?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))?;
    let claimed = Claimed {
        path: path.to_owned(),
        identity: identity(path).context("reading the socket just bound")?,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
        .with_context(|| format!("setting permissions on {}", path.display()))?;
    Ok((listener, claimed))
}

/// The socket of a running daemon for `unit`, or for the only daemon
/// running when `unit` is `None`.  `None` when no daemon answers; an
/// error when one answers but may not be used, or several do.
pub(crate) fn find(unit: Option<&str>) -> Result<Option<PathBuf>> {
    let directories: Vec<PathBuf> = user_directory()
        .into_iter()
        .chain([PathBuf::from(SYSTEM_SOCKET_DIRECTORY)])
        .collect();
    find_in(&directories, unit)
}

fn find_in(directories: &[PathBuf], unit: Option<&str>) -> Result<Option<PathBuf>> {
    let mut live = Vec::new();
    for directory in directories {
        let candidates = match unit {
            Some(unit) => vec![socket_path(directory, unit)],
            None => sockets_in(directory),
        };
        for path in candidates {
            match UnixStream::connect(&path) {
                Ok(_) => live.push(path),
                Err(error) if error.kind() == ErrorKind::PermissionDenied => bail!(
                    "a daemon answers at {}, but this user may not connect",
                    path.display()
                ),
                // Absent, or left behind by a daemon that died.
                Err(_) => {}
            }
        }
    }
    match live.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        several => {
            let serials: Vec<_> = several
                .iter()
                .filter_map(|path| path.file_stem()?.to_str())
                .collect();
            bail!(
                "daemons for several units ({}); choose one with --unit",
                serials.join(", ")
            )
        }
    }
}

fn sockets_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut sockets: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == SOCKET_EXTENSION)
        })
        .collect();
    sockets.sort();
    sockets
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::claim;
    use super::find_in;
    use super::lock_in;
    use super::socket_path;

    #[tokio::test]
    async fn claims_finds_and_releases_a_socket() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-socket-{}", std::process::id()));
        let path = socket_path(&directory, "111");
        let directories = [directory.clone()];
        assert_eq!(find_in(&directories, None).expect("none"), None);
        let (listener, claimed) = claim(&path).expect("claim");
        assert!(claim(&path).is_err(), "a live daemon keeps its socket");
        assert_eq!(
            find_in(&directories, None).expect("one"),
            Some(path.clone())
        );
        assert_eq!(
            find_in(&directories, Some("111")).expect("by unit"),
            Some(path.clone())
        );
        assert_eq!(find_in(&directories, Some("222")).expect("other"), None);
        drop(listener);
        // A dead daemon's leftover file is not a daemon, and is reclaimed.
        assert_eq!(find_in(&directories, None).expect("stale"), None);
        let (listener, reclaimed) = claim(&path).expect("reclaim");
        let second = socket_path(&directory, "222");
        let (other, other_claimed) = claim(&second).expect("second unit");
        assert!(find_in(&directories, None).is_err(), "several need --unit");
        drop((listener, other, claimed, other_claimed));
        drop(reclaimed);
        assert!(!path.exists() && !second.exists());
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn one_process_holds_a_unit() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-lock-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("directory");
        let held = lock_in(&directory, "111").expect("first");
        assert!(lock_in(&directory, "111").is_err(), "held");
        assert!(lock_in(&directory, "222").is_ok(), "another unit is free");
        drop(held);
        assert!(lock_in(&directory, "111").is_ok(), "released");
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[tokio::test]
    async fn leaves_a_later_daemons_socket_alone() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-identity-{}", std::process::id()));
        let path = socket_path(&directory, "111");
        let (first_listener, first) = claim(&path).expect("first");
        drop(first_listener);
        // A dead daemon's file is reclaimed by the next one.
        let (_second_listener, second) = claim(&path).expect("second");
        drop(first);
        assert!(path.exists(), "the first guard left the second socket");
        drop(second);
        assert!(!path.exists());
        fs::remove_dir_all(&directory).expect("cleanup");
    }
}
