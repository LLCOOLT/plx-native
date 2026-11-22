//! Physically owned Live TV model (`crate::livetv`) and its machine.
//!
//! The state's IO leaves as work on its outbox (`crate::livetv::work`), taken by the app's runner
//! after each step; the answers are plain channels, not `tape`-admitted content resources: nothing
//! a recording replays reaches Live TV, and under a controlled input (an active tape) the state
//! asks for no work at all, so a replay stays deterministic. The step reads neither the clock nor
//! the session: both arrive as arguments.

use crate::livetv::work::{LiveTvInput, Work};
use crate::livetv::{LiveTvCmd, LiveTvState, LiveTvView};
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::StoreEv;

#[derive(Default)]
pub struct LiveTvStore {
    state: LiveTvState,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl LiveTvStore {
    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }
    pub fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }
    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }
    pub fn view(&self) -> LiveTvView<'_> {
        self.state.view()
    }
    pub fn run(&mut self, cmd: LiveTvCmd, input: &LiveTvInput, now_ms: i64) -> bool {
        let changed = self.state.run(cmd, input, now_ms);
        if changed {
            self.bump();
        }
        changed
    }
    pub fn pump(&mut self, input: &LiveTvInput, now_ms: i64) -> bool {
        let changed = self.state.pump(input, now_ms);
        if changed {
            self.bump();
        }
        changed
    }

    /// The IO the last steps asked for, for the app's runner (`crate::livetv::work`).
    pub fn take_work(&mut self) -> Vec<Work> {
        self.state.take_work()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(&mut self, origin: &str, lineup: crate::livetv::guide::Lineup, now_ms: i64) {
        self.state.install_for_test(origin, lineup, now_ms);
        self.bump();
    }
}

impl<H: Host> Machine<H> for LiveTvStore {
    type Ev = StoreEv<LiveTvCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        // Unreached: nothing steps this store as a Machine (`Bridge::livetv_run` and
        // `Bridge::livetv_pump` call `run`/`pump` with the input and clock they captured, then
        // start the work). A step reads no registry and no clock, and this one is handed neither.
        let _ = ev;
        debug_assert!(false, "a Live TV step was handed no input");
        plx_base::eventlog::log("livetv: a step reached the store with no input - dropped");
        Handled::Yes
    }
}
