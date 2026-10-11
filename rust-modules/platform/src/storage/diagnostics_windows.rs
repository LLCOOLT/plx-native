//! The storage evidence file, compiled out of the Windows desktop build.
//!
//! The unix module (`diagnostics.rs`) publishes the television's storage-helper outcomes beside
//! uid/gid/mode/mount probes of the webOS directories a session file can live in. None of that
//! exists on Windows: the helper protocol is unsupported off the television (`client::transact`
//! answers `Unsupported`), and the probes are POSIX identity and mount facts. So this keeps the
//! same entry points and publishes nothing; the erase sweep's [`finish_disable`] still removes a
//! file of that name if one is there.

use super::wire::failure::{Detail, HelperFailure};
use std::path::Path;

pub const NAME: &str = plx_base::paths::runtime_file::STORAGE_DIAGNOSTICS;

pub fn start(_version: &'static str) {}

pub fn helper(_failure: Option<HelperFailure>, _attempts: u32, _elapsed_ms: u64) {}

pub fn activation(_detail: Detail, _elapsed_ms: u64) {}

pub fn disable() {}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_test() {}

pub fn finish_disable(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(root.join(NAME)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{NAME}: {error}")),
    }
}
