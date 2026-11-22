//! **Live TV's IO leaves its machines as [`Work`]** (AGENTS.md, "Logic never performs IO";
//! `docs/sans-io-guide.md`). The Live TV store, its virtual channels (`crate::vchannel::channels`)
//! and its Tunarr keeper (`tunarr_keep`) start no worker, read no process-global registry and have
//! no `cfg(test)` fork: a step that needs a fetch or a write pushes a [`Work`] onto its
//! [`Outbox`], and what a step needs to know about the session it reads from the
//! [`LiveTvInput`] the app captured for it. The app's runner (`app::adapters::livetv_work`)
//! drains the outbox after each step, starts each item on the lane it names, and hands its body
//! the [`Reach`] seam, the one place a body asks which servers are signed in. Answers come back
//! through the channel the step kept, drained by the next pump on the frame thread.
//!
//! Under a controlled run (a tape replaying, or a test) the input says so and the steps start no
//! work at all, exactly as before the move, so a replay stays deterministic.

use plx_plex::plex::{Client, ServerId};

/// What a Live TV step knows about the session, captured by the app before the step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveTvInput {
    /// A tape replays or the run is scripted: start no work (module doc).
    pub controlled: bool,
    /// The current server.
    pub server: ServerId,
    /// The current profile's key (`session::current_profile_key`).
    pub profile: String,
    /// The server roster's generation (`plex::server_roster_gen`).
    pub roster: u32,
    /// The channel last tuned, as the session holds it (`Session::livetv_channel`).
    pub remembered_channel: String,
}

/// The seam a [`Work`] body reaches the signed-in servers through. The app implements it over
/// the server registry; a body never names the registry itself.
pub trait Reach {
    /// Every registered server.
    fn servers(&self) -> Vec<ServerId>;
    /// The current server.
    fn current(&self) -> ServerId;
    /// The client for `sid`, when it is registered.
    fn client(&self, sid: ServerId) -> Option<&'static Client>;
    /// The server whose machine identifier is `machine_id`, when it is signed in.
    fn machine(&self, machine_id: &str) -> Option<ServerId>;
}

/// Where a [`Work`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// A small worker thread: a network read or write.
    Worker,
    /// The storage worker: a session or file write that must not block the frame.
    Storage,
}

/// One piece of IO a step asked for. The body owns everything it needs and answers through the
/// channel its step kept; it is handed the [`Reach`] seam when it runs.
pub struct Work {
    pub name: &'static str,
    pub lane: Lane,
    pub body: Box<dyn FnOnce(&dyn Reach) + Send>,
}

impl std::fmt::Debug for Work {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Work").field("name", &self.name).field("lane", &self.lane).finish_non_exhaustive()
    }
}

/// The work a machine asked for and the runner has not taken yet.
#[derive(Debug, Default)]
pub struct Outbox {
    items: Vec<Work>,
}

impl Outbox {
    pub fn worker(&mut self, name: &'static str, body: impl FnOnce(&dyn Reach) + Send + 'static) {
        self.items.push(Work { name, lane: Lane::Worker, body: Box::new(body) });
    }

    pub fn storage(&mut self, name: &'static str, body: impl FnOnce(&dyn Reach) + Send + 'static) {
        self.items.push(Work { name, lane: Lane::Storage, body: Box::new(body) });
    }

    /// Everything asked for so far, oldest first.
    pub fn take(&mut self) -> Vec<Work> {
        std::mem::take(&mut self.items)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The names of the queued items, oldest first (tests).
    pub fn names(&self) -> Vec<&'static str> {
        self.items.iter().map(|w| w.name).collect()
    }
}
