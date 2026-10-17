//! The dev live UI capture stream, compiled out of the Windows desktop build.
//!
//! `capture.rs` serves the app's own frames over a raw TCP socket to
//! `tools/stream-screen.py --source app`, so a developer can watch a television they are not in
//! front of. On a desktop the window itself is the view, and the stream's socket and
//! `libturbojpeg` paths are unix-only, so these are the same three entry points doing nothing.

pub(crate) fn init() {}

pub(crate) fn shutdown() {}

pub(crate) fn tick(_now: u32) {}
