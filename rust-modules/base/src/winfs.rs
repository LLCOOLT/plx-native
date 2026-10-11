//! The Windows counterparts of the unix file-open guards, for the desktop build's `cfg(windows)`
//! arms.
//!
//! The unix code opens private files with `O_NOFOLLOW` and then checks the opened descriptor's
//! owner uid, link count and mode. Windows has no uid or mode, and the desktop build keeps its files
//! under the user's own profile, whose ACL already keeps other users out — so the arms that use
//! this keep only the half that still means something there: never follow a symlink or junction
//! at the final name, and refuse anything that is not then a regular file.

use std::os::windows::fs::OpenOptionsExt;

/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a symlink or junction ITSELF rather than its target.
pub const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// The `O_NOFOLLOW` counterpart. A link opened this way reports as a symlink, not a file or a
/// directory, so the caller's `is_file()`/`is_dir()` check is what refuses it.
pub fn no_follow(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
}
