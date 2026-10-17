//! Activation correlation, compiled out of the Windows desktop build.
//!
//! The unix module (`activation.rs`) correlates an LS2 activation of the television's storage
//! helper with the helper's own start-failure record through private 0700/0600 files. There is no
//! helper to activate on Windows (`client::transact` answers `Unsupported`), so an attempt is
//! never begun or captured and no failure is ever read back.

use super::failure::Detail;
use std::path::Path;

#[derive(Clone)]
pub struct Attempt {
    nonce: String,
}

impl Attempt {
    pub fn begin(_root: &Path) -> Option<Self> {
        None
    }

    pub fn capture(_root: &Path) -> Option<Self> {
        None
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn record_failure(&self, _root: &Path, _detail: Detail) {}
}

pub fn read_failure(_root: &Path, _nonce: &str) -> Option<Detail> {
    None
}
