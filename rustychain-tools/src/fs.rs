//! File-system support shared by the write and edit executors: bounded reads,
//! atomic writes and the error shape both report (FR-TOOL-FILE-1, FR-TOOL-FILE-3).

use std::fs::OpenOptions;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustychain_core::{Error, Result};

/// How many temporary names to try before giving a collision up for good.
const TEMPORARY_ATTEMPTS: usize = 8;

/// Reads `path`, refusing anything larger than `max_bytes`.
///
/// The size check is taken from the bytes actually read, not from metadata, so a
/// file that grows between the stat and the read cannot slip past the limit.
pub(crate) fn read_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|error| io_error(path, &error))?;
    let cap = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(cap)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, &error))?;
    if bytes.len() > max_bytes {
        return Err(Error::LimitExceeded {
            what: path.display().to_string(),
            limit: max_bytes,
        });
    }
    Ok(bytes)
}

/// Reads `path` as UTF-8 text, refusing anything larger than `max_bytes` and
/// anything that is not valid UTF-8.
pub(crate) fn read_text(path: &Path, max_bytes: usize) -> Result<String> {
    let bytes = read_bytes(path, max_bytes)?;
    String::from_utf8(bytes)
        .map_err(|_| Error::InvalidInput(format!("{} is not valid UTF-8", path.display())))
}

/// Writes `bytes` onto `path` through a sibling temporary file that is renamed
/// into place, so an interrupted call never leaves a half-written file and a
/// reader sees either the old file or the new one. Parent directories are
/// created and the temporary is removed when anything fails. An overwritten
/// file keeps its permissions (FR-TOOL-FILE-1).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = stage(path, bytes)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_error(path, &error));
    }
    sync_parent(path);
    Ok(())
}

/// Writes `bytes` onto `path` only if nothing is there, and says whether it
/// wrote. The link *is* the check: a writer that looks first and then renames
/// lets the loser of a race silently replace the winner, because `rename`
/// always clobbers (FR-TOOL-WRITE-1). A filesystem where a hard link is
/// impossible cannot give that guarantee at all, and says so rather than
/// quietly doing the racy thing.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<bool> {
    let temporary = stage(path, bytes)?;
    let created = match std::fs::hard_link(&temporary, path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!(
                    "{}: this filesystem cannot create a file without a hard link; \
                     pass overwrite=true to create or replace it",
                    path.display()
                ),
            )));
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(io_error(path, &error));
        }
    };
    let _ = std::fs::remove_file(&temporary);
    sync_parent(path);
    Ok(created)
}

/// Names an error after the file it is about: `std::io::Error` alone loses the
/// path, and the path is what the model needs to retry.
pub(crate) fn io_error(path: &Path, error: &std::io::Error) -> Error {
    Error::Io(std::io::Error::new(
        error.kind(),
        format!("{}: {error}", path.display()),
    ))
}

/// Places `bytes` in a sibling temporary of `path` and answers with its name,
/// parents created. The temporary is opened with `create_new`, so it is never
/// written through a name that is already there: a symbolic link planted at a
/// guessed name would otherwise have this write truncate whatever it points at.
/// The name carries a random part, so guessing it in advance takes more than
/// reading the process id (FR-TOOL-FILE-1).
fn stage(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, &error))?;
    }
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let temporary = temporary_path(path);
        match place(&temporary, path, bytes) {
            Ok(()) => return Ok(temporary),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt == TEMPORARY_ATTEMPTS {
                    return Err(io_error(path, &error));
                }
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(io_error(path, &error));
            }
        }
    }
}

fn place(temporary: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Ok(metadata) = std::fs::metadata(target) {
        let _ = std::fs::set_permissions(temporary, metadata.permissions());
    }
    drop(file);
    Ok(())
}

/// Makes the new name durable where the platform allows a directory to be
/// synced: the content is flushed before the name appears, and this flushes the
/// directory entry afterwards, so a power cut cannot lose a file that was
/// already reported written. A platform that will not open a directory simply
/// has nothing more to give.
fn sync_parent(path: &Path) {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!(
        ".{name}.{}.{}.rustychain-tmp",
        std::process::id(),
        ticket()
    ))
}

/// A number that cannot be guessed before the call and differs on every one:
/// the counter is hashed with the hasher's per-process random seed.
fn ticket() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ticket = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "rustychain-fs-{name}-{}-{ticket}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("scratch");
        directory
    }

    #[test]
    fn a_file_over_the_limit_is_refused_by_name() {
        let directory = scratch("limit");
        let path = directory.join("big.txt");
        std::fs::write(&path, "0123456789").expect("write");
        let error = read_text(&path, 4).expect_err("over the limit");
        assert!(matches!(error, Error::LimitExceeded { limit: 4, .. }));
    }

    #[test]
    fn a_file_that_is_not_utf8_is_refused_by_name() {
        let directory = scratch("utf8");
        let path = directory.join("bytes.bin");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).expect("write");
        let error = read_text(&path, 1024).expect_err("not utf8");
        assert!(error.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn a_missing_file_names_the_path_in_the_error() {
        let directory = scratch("missing");
        let path = directory.join("absent.txt");
        let error = read_text(&path, 1024).expect_err("missing");
        assert!(error.to_string().contains("absent.txt"));
    }

    #[test]
    fn an_atomic_write_leaves_no_temporary_behind() {
        let directory = scratch("atomic");
        let path = directory.join("kept.txt");
        write_atomic(&path, b"first").expect("write");
        write_atomic(&path, b"second").expect("rewrite");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "second");
        let entries: Vec<String> = std::fs::read_dir(&directory)
            .expect("dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["kept.txt".to_owned()]);
    }

    #[test]
    fn a_write_creates_the_parent_directories() {
        let directory = scratch("parents");
        let path = directory.join("a").join("b").join("file.txt");
        write_atomic(&path, b"nested").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "nested");
    }

    #[test]
    fn a_creation_writes_when_nothing_is_there() {
        let directory = scratch("new");
        let path = directory.join("fresh.txt");
        let created = write_new(&path, b"first").expect("write");
        assert!(created);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "first");
        let entries: Vec<String> = std::fs::read_dir(&directory)
            .expect("dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["fresh.txt".to_owned()], "no temporary");
    }

    #[test]
    fn a_creation_that_loses_the_race_leaves_the_winner_alone() {
        let directory = scratch("race");
        let path = directory.join("contested.txt");
        std::fs::write(&path, "the winner\n").expect("seed");

        let created = write_new(&path, b"the loser\n").expect("write");

        assert!(!created, "the file was already there");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "the winner\n",
            "a creation never replaces, even when it decides after the fact"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_temporary_is_never_written_through_an_existing_name() {
        let directory = scratch("symlink");
        let temporary = directory.join("planted.rustychain-tmp");
        let victim = directory.join("victim.txt");
        std::fs::write(&victim, "untouched\n").expect("victim");
        std::os::unix::fs::symlink(&victim, &temporary).expect("planted link");

        let error = place(&temporary, &victim, b"payload").expect_err("refused");

        assert_eq!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists,
            "the name is claimed with create_new, not opened through"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read"),
            "untouched\n",
            "whatever a planted link pointed at is still there"
        );
    }

    #[test]
    fn the_temporary_name_is_not_the_target_and_is_not_a_counter() {
        let directory = scratch("names");
        let path = directory.join("file.txt");
        let first = temporary_path(&path);
        let second = temporary_path(&path);

        assert_eq!(first.parent(), path.parent());
        assert_ne!(first, path);
        assert_ne!(first, second, "two names in a row differ");
        assert!(
            first.to_string_lossy().contains(".rustychain-tmp"),
            "a leftover is recognizable: {}",
            first.display()
        );
    }
}
