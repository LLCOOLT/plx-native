//! The [`JsonStore`](super::JsonStore) file primitives for the Windows desktop build.
//!
//! The unix primitives in `mod.rs` hold the state root open and do every child operation relative
//! to that descriptor (`openat`/`renameat`/`unlinkat`), refuse symlinks with `O_NOFOLLOW`, and
//! check each file's owner uid, link count and mode, because on the television the root sits in a
//! directory another app in the same group can write. These are the same operations by path, with
//! the closest Windows equivalent of each guard, and they are weaker in these stated ways:
//!
//! * **No owner or mode checks.** The desktop state root is under the user's own profile, whose
//!   ACL already keeps other users out; there is no uid to compare and no mode to repair, so a
//!   legacy read is always `trusted` and nothing is `chmod`ed.
//! * **The root is pinned by share mode, not by descriptor-relative I/O.** The root handle is opened
//!   without `FILE_SHARE_DELETE`, so while the store holds it nobody can rename or delete the
//!   directory; that is what makes the path-based child operations below land in the directory
//!   that was validated, and why `check_root` has no inode to compare.
//! * **Symlinks and junctions are refused** by opening with `FILE_FLAG_OPEN_REPARSE_POINT` (the
//!   reparse point itself, not its target) and then requiring a regular file or a directory.
//! * **No directory fsync.** Windows has no user-mode flush of a directory; [`Dir::sync_all`] is a
//!   no-op and a commit's durability after the rename is NTFS's metadata journal's.

use super::{io_error, CommitStage, ReadFailure, StoreError, MAX_BYTES};
use plx_base::winfs::FILE_FLAG_OPEN_REPARSE_POINT;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// Lets `CreateFileW` open a directory at all.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// The validated state root: its path, and a handle that pins it in place (see the module doc).
pub(super) struct Dir {
    path: PathBuf,
    handle: File,
}

impl Dir {
    pub(super) fn metadata(&self) -> io::Result<Metadata> {
        self.handle.metadata()
    }

    /// No directory flush exists in user mode on Windows; see the module doc.
    pub(super) fn sync_all(&self) -> io::Result<()> {
        Ok(())
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

fn no_follow() -> OpenOptions {
    let mut options = OpenOptions::new();
    plx_base::winfs::no_follow(&mut options);
    options
}

/// See `mod.rs`'s unix twin. Always `trusted`: there are no group/other write bits to distrust.
pub fn read_owned_bytes(path: &Path) -> Result<Option<(Vec<u8>, bool)>, StoreError> {
    let mut file = match no_follow().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(CommitStage::Readback, error)),
    };
    let meta = file
        .metadata()
        .map_err(|error| io_error(CommitStage::Readback, error))?;
    if !meta.is_file() {
        return Err(StoreError::RecordNotRegular);
    }
    if meta.len() > MAX_BYTES {
        return Err(StoreError::TooLarge);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(CommitStage::Readback, error))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(StoreError::TooLarge);
    }
    Ok(Some((bytes, true)))
}

pub(super) fn validate_root(root: &Path) -> Result<Dir, StoreError> {
    let handle = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(root)
        .map_err(|e| io_error(CommitStage::ParentOpen, e))?;
    validate_root_metadata(
        &handle
            .metadata()
            .map_err(|e| io_error(CommitStage::ParentOpen, e))?,
    )?;
    Ok(Dir { path: root.to_owned(), handle })
}

/// A directory, and not a reparse point (which reports as a symlink, not a directory).
pub(super) fn validate_root_metadata(meta: &Metadata) -> Result<(), StoreError> {
    if !meta.is_dir() {
        return Err(StoreError::RootNotDirectory);
    }
    Ok(())
}

/// `flags` are the `O_*` values the unix call sites pass: a create is `O_WRONLY | O_CREAT |
/// O_EXCL`, anything else is a read. `mode` has no Windows meaning.
pub(super) fn open_at(directory: &Dir, name: &str, flags: i32, _mode: u32) -> io::Result<File> {
    let mut options = no_follow();
    if flags & libc::O_CREAT != 0 {
        options.write(true).create_new(true);
    } else {
        options.read(true);
    }
    options.open(directory.child(name))
}

pub(super) fn remove_at(directory: &Dir, name: &str) {
    let _ = std::fs::remove_file(directory.child(name));
}

pub(super) fn unlink_at(directory: &Dir, name: &str) -> io::Result<()> {
    std::fs::remove_file(directory.child(name))
}

pub(super) fn directory_names(directory: &Dir) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&directory.path)? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
}

/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`: replaces the destination in one step.
pub(super) fn rename_at(directory: &Dir, from: &str, to: &str) -> io::Result<()> {
    std::fs::rename(directory.child(from), directory.child(to))
}

pub(super) fn validate_destination(
    directory: &Dir,
    name: &str,
    _replacing_cleared: bool,
) -> Result<(), StoreError> {
    let file = match open_at(directory, name, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error(CommitStage::CreateTemp, e)),
    };
    let meta = file
        .metadata()
        .map_err(|e| io_error(CommitStage::CreateTemp, e))?;
    if !meta.is_file() {
        return Err(StoreError::DestinationNotRegular);
    }
    Ok(())
}

pub(super) fn read_owned_regular(directory: &Dir, name: &str) -> Result<Vec<u8>, ReadFailure> {
    let mut file = open_at(directory, name, libc::O_RDONLY, 0).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            ReadFailure::Missing
        } else {
            ReadFailure::Error(io_error(CommitStage::Readback, e))
        }
    })?;
    let meta = file
        .metadata()
        .map_err(|e| ReadFailure::Error(io_error(CommitStage::Readback, e)))?;
    if !meta.is_file() {
        return Err(ReadFailure::Error(StoreError::RecordNotRegular));
    }
    if meta.len() > MAX_BYTES {
        return Err(ReadFailure::Error(StoreError::TooLarge));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ReadFailure::Error(io_error(CommitStage::Readback, e)))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ReadFailure::Error(StoreError::TooLarge));
    }
    Ok(bytes)
}

/// A fresh, exclusively created temporary beside `destination`. The 32 hex digits come from std's
/// per-process randomly keyed `RandomState` (seeded from the OS generator) rather than
/// `/dev/urandom`; they only need to be unguessable enough not to collide, and `create_new` makes
/// a collision a retry rather than a reuse.
pub(super) fn create_temp(
    directory: &Dir,
    destination: &str,
    mut injected: impl FnMut(CommitStage) -> Option<i32>,
) -> Result<(String, File), StoreError> {
    use std::hash::{BuildHasher, Hasher};
    if let Some(errno) = injected(CommitStage::CreateTemp) {
        return Err(StoreError::Io {
            stage: CommitStage::CreateTemp,
            errno,
        });
    }
    for _ in 0..16 {
        let half = || std::collections::hash_map::RandomState::new().build_hasher().finish();
        let name = format!(".{destination}.new-{:016x}{:016x}", half(), half());
        match open_at(directory, &name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600) {
            Ok(file) => return Ok((name, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(CommitStage::CreateTemp, e)),
        }
    }
    Err(StoreError::Io {
        stage: CommitStage::CreateTemp,
        errno: libc::EEXIST,
    })
}
