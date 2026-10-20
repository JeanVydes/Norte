//! File-system support shared by the write and edit executors: bounded reads,
//! atomic writes and the error shape both report (FR-TOOL-FILE-1, FR-TOOL-FILE-3).

use std::fs::OpenOptions;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use norte_core::{Error, Result};

/// How many symbolic links a write follows before it gives the name up as a
/// loop and refuses the call.
const MAX_LINK_HOPS: usize = 8;
/// How many temporary names to try before giving a collision up for good.
const TEMPORARY_ATTEMPTS: usize = 8;

/// Reads `path`, refusing anything larger than `max_bytes`.
///
/// The path must be a regular file. Opening a FIFO with no writer blocks
/// forever, and a socket or device can block or produce unbounded input, so
/// those are refused by type before the open: a tool call must not be able to
/// wedge the turn.
///
/// The size check is taken from the bytes actually read, not from metadata, so a
/// file that grows between the stat and the read cannot slip past the limit.
pub(crate) fn read_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    require_regular_file(path)?;
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

/// Refuses a path that is not a regular file.
///
/// `metadata` follows symbolic links, so a link to a FIFO is refused too. The
/// check is deliberately before the open: checking after would already be
/// blocked inside `open(2)`.
fn require_regular_file(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|error| io_error(path, &error))?;
    if metadata.file_type().is_file() {
        return Ok(());
    }
    Err(Error::InvalidInput(format!(
        "{} is not a regular file; FIFOs, sockets and devices are refused because they can block \
         a tool indefinitely",
        path.display()
    )))
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
    let path = &followed(path)?;
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
    let path = &followed(path)?;
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

/// The name a write lands on: when the last component of `path` is a symbolic
/// link, the link is followed and what it points at is what gets written, so
/// editing a linked file never replaces the link with a copy of the file — what
/// a dotfile manager, a linked configuration or a checkout of symlinked sources
/// would lose. Only that last component is resolved: which names may be reached
/// through a directory is still the host's business, not this crate's
/// (FR-POL-1).
fn followed(path: &Path) -> Result<PathBuf> {
    let mut target = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let Ok(metadata) = std::fs::symlink_metadata(&target) else {
            return Ok(target);
        };
        if !metadata.file_type().is_symlink() {
            return Ok(target);
        }
        let Ok(link) = std::fs::read_link(&target) else {
            return Ok(target);
        };
        target = if link.is_absolute() {
            link
        } else {
            target
                .parent()
                .map_or(link.clone(), |parent| parent.join(&link))
        };
    }
    Err(io_error(
        path,
        &std::io::Error::other("too many symbolic links to follow"),
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
        ".{name}.{}.{}.norte-tmp",
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
        let directory =
            std::env::temp_dir().join(format!("norte-fs-{name}-{}-{ticket}", std::process::id()));
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

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_instead_of_blocking_the_read() {
        let directory = scratch("fifo");
        let path = directory.join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo");
        assert!(status.success());
        let error = read_text(&path, 1024).expect_err("a FIFO must be refused");
        assert!(error.to_string().contains("not a regular file"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
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
    fn a_plain_path_is_its_own_target() {
        let directory = scratch("plain");
        let path = directory.join("main.rs");
        std::fs::write(&path, "one\n").expect("seed");
        assert_eq!(followed(&path).expect("followed"), path);
        assert_eq!(
            followed(&directory.join("absent.rs")).expect("followed"),
            directory.join("absent.rs")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_write_follows_a_symbolic_link_instead_of_replacing_it() {
        let directory = scratch("symlink");
        let target = directory.join("real.rs");
        std::fs::write(&target, "old\n").expect("seed");
        let link = directory.join("link.rs");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        write_atomic(&link, b"new\n").expect("write");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "new\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("metadata")
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_creation_through_a_link_makes_its_target() {
        let directory = scratch("symlink-create");
        let target = directory.join("made.rs");
        let link = directory.join("link.rs");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(write_new(&link, b"made\n").expect("create"));
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "made\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("metadata")
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_chain_of_links_is_followed_to_its_end() {
        let directory = scratch("chain");
        let target = directory.join("real.rs");
        std::fs::write(&target, "old\n").expect("seed");
        let middle = directory.join("middle.rs");
        std::os::unix::fs::symlink("real.rs", &middle).expect("symlink");
        let first = directory.join("first.rs");
        std::os::unix::fs::symlink("middle.rs", &first).expect("symlink");
        write_atomic(&first, b"new\n").expect("write");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "new\n");
        for link in [&middle, &first] {
            assert!(
                std::fs::symlink_metadata(link)
                    .expect("metadata")
                    .file_type()
                    .is_symlink(),
                "{} is still a link",
                link.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_chain_that_never_ends_is_refused() {
        let directory = scratch("loop");
        let mut previous = directory.join("end.rs");
        std::fs::write(&previous, "old\n").expect("seed");
        for step in 0..MAX_LINK_HOPS {
            let link = directory.join(format!("link-{step}.rs"));
            std::os::unix::fs::symlink(&previous, &link).expect("symlink");
            previous = link;
        }
        let error = write_atomic(&previous, b"new\n").expect_err("a loop");
        assert!(
            error.to_string().contains("too many symbolic links"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("end.rs")).expect("read"),
            "old\n"
        );
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
        let temporary = directory.join("planted.norte-tmp");
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
            first.to_string_lossy().contains(".norte-tmp"),
            "a leftover is recognizable: {}",
            first.display()
        );
    }
}
