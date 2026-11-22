//! **Live TV's half of the loop**: what the Live TV page and the player's live banner ask for
//! (`LiveTvReq`), performed with the playback session, the adapter and the navigation a screen may
//! not name (§2.1).
//!
//! A TUNE is three steps, the middle one on a worker:
//!
//! 1. Stop whatever is playing (a film, or the previous channel — closing its connection first, so
//!    Tunarr can hand the transcode to the next viewer or keep it warm for a quick zap back), mark
//!    the session as this channel TUNING (`route::set_live_tuning`), and put the player on screen
//!    with the banner saying so.
//! 2. Probe the channel's stream (`plx_media::live::probe`): its codecs and frame rate, which the
//!    Load payload must declare.
//! 3. When the probe lands — and only if the session is still on that channel — install the
//!    channel as the playback (`route::install_live_stream`) and start the engine. A failed probe
//!    leaves the banner saying the channel is unavailable, and OK retries.
//!
//! A live stream that ENDS (Tunarr restarted, the network dropped) is re-tuned rather than left,
//! a bounded number of times; an engine failure is retried the same way before the failure
//! read-out is allowed to stand.
//!
//! A **virtual channel** (`plxvc:`, `plx_data::vchannel`) has no stream to probe: step 2 reads
//! what its timeline airs right now and plays that library item from the moment the channel is
//! at (`route::request_play_channel` — quiet, so nothing reaches the profile's history), under
//! the same Live TV session, so the banner, CH▲/▼, the surf list and *last channel* all work. A
//! programme that ends is the channel moving on: it is tuned again at once, which plays the next
//! one. A pause falls behind live; tuning the channel again is *Jump to live*.
//!
//! A **Tunarr channel in native mode** (`plx_data::livetv::native`) is played the same way. Before
//! its stream is probed, Tunarr is asked what the channel airs now; when that is an item on a Plex
//! server the app is signed into, the item's own file plays from the channel's point (Tunarr's
//! clock, mapped onto ours), and when the channel leaves it (the FILE position Tunarr's schedule
//! cuts it at, or the file's end) Tunarr is asked again. Anything else — flex, another media source,
//! an older Tunarr without the route, a re-tune after a failure — plays the `.ts` stream exactly as
//! before; flex is asked about again when it ends.

use super::bridge::{self, AppHost};
use plx_machine::machine::{AppCommandOutcome, InputOwner, MachineId};
use plx_data::livetv::native::{self, Plan, Why};
use plx_media::live::{LiveSession, ProbeError, StreamFacts};
use plx_plex::plex::ServerId;
use plx_screens::registry::{AppArg, HomeTab, LiveTvReq};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

/// How many times a dropped or failed channel is re-tuned on its own before the viewer is asked.
const AUTO_RETRIES: u32 = 3;
/// The wait before each automatic re-tune, in frame-clock ms.
const RETRY_DELAY_MS: u32 = 2_000;

struct Pending {
    /// Which tune this probe belongs to; a newer tune supersedes it.
    epoch: u64,
    url: String,
    rx: Receiver<Result<StreamFacts, ProbeError>>,
}

/// A native-mode question in flight: what a Tunarr channel airs, and whether the app can play it.
struct NativePending {
    epoch: u64,
    url: String,
    rx: Receiver<(Plan, Option<(String, ServerId)>)>,
}

/// The tuner: the probe in flight, and the automatic re-tune schedule.
#[derive(Default)]
pub(crate) struct LiveTuner {
    epoch: u64,
    pending: Option<Pending>,
    native_pending: Option<NativePending>,
    /// Tunarr origins that answered `native-playback` with 404/405: an older Tunarr, asked no more.
    native_unsupported: std::collections::HashSet<String>,
    /// Which signed-in server each Tunarr media source turned out to be (`native::resolve`).
    native_sources: std::collections::HashMap<String, ServerId>,
    /// The Tunarr programme playing natively, and the FILE position (ms) the channel leaves it at.
    native_program: Option<String>,
    native_end_ms: Option<i64>,
    /// The programme that just ended natively: asked about again and still on (the file was
    /// shorter than Tunarr's schedule, or the clocks disagree by a moment), its tail plays from
    /// Tunarr's stream instead of re-tuning into the same end.
    native_ended: Option<String>,
    /// A `.ts` stream playing over flex: when (local wall ms) to ask Tunarr again.
    native_recheck_at: Option<i64>,
    /// Automatic re-tunes spent on the current channel since it last played.
    retries: u32,
    /// When to re-tune on our own (frame-clock ms), if anything is owed.
    retry_at: Option<u32>,
    /// What each channel's probe found, by stream URL, so tuning a channel again starts it at once
    /// instead of reading seconds of its stream first. Tunarr encodes every channel to its one
    /// transcode configuration, so the answer holds; a re-tune after a failure probes afresh.
    known: std::collections::HashMap<String, StreamFacts>,
}

/// What the tuner works on: the pages, the Bridge, the player and its adapters, and the tuner's
/// own state. The command runner builds one from its `CommandResources` to perform a request; the
/// loop builds one from the `App` for the per-frame pump.
pub(crate) struct Tune<'a, 'o> {
    pub(crate) pages: &'a mut plx_ui::dispatch::Dispatcher<AppHost>,
    pub(crate) bridge: &'a mut bridge::Bridge<'o>,
    pub(crate) player: &'a mut plx_media::player::machine::Player,
    pub(crate) adapters: &'a mut super::Adapters,
    pub(crate) tuner: &'a mut LiveTuner,
}

impl Tune<'_, '_> {
    /// Which page is on top (`App::route`).
    fn route(&self) -> AppArg {
        self.pages.top_arg().cloned().unwrap_or(AppArg::Home)
    }

    /// The tuner changed what the page shows (`App::note_presentation_damage`).
    fn note_presentation_damage(&mut self, why: plx_machine::present::Provenance) {
        self.pages.present.note_in_scope(plx_machine::present::Scope::Page,
            plx_machine::present::PresentEvent::Damage(why));
    }
}

/// Run `f` on the loop's [`Tune`] over `app` (the per-frame pump and the stream's end).
pub(crate) fn with_app<R>(app: &mut super::App, f: impl FnOnce(&mut Tune<'_, '_>) -> R) -> R {
    let mut bridge = app.runtime.bridge(&mut app.bridge);
    let mut t = Tune { pages: &mut app.pages, bridge: &mut bridge, player: &mut app.player,
        adapters: &mut app.adapters, tuner: &mut app.livetv };
    f(&mut t)
}

/// The declaration a tune of `url` may start with, skipping the probe: one already measured,
/// unless this is a re-tune after a failure (`retries > 0`), which must not trust it.
fn known_facts(known: &std::collections::HashMap<String, StreamFacts>, url: &str, retries: u32) -> Option<StreamFacts> {
    (retries == 0).then(|| known.get(url).copied()).flatten()
}

/// **Perform one Live TV request a screen raised** (`ExternalAppFx::LiveTv`, and Home's On Now
/// card), in the command runner that took it: the tuner, the player and the pages arrive in `t`.
/// A request from a page that no longer holds the input is refused.
pub(crate) fn execute_livetv_command(t: &mut Tune<'_, '_>, source: MachineId, request: LiveTvReq,
    ret: plx_ui::screen::ReturnState<u32, plx_screens::registry::PageMemory>, now: u32,
) -> AppCommandOutcome {
    // Settings > Live TV comes from a page inside the Settings surface, whose inner stack is not
    // the container's entry; it only navigates, so it needs no owner check.
    if request == LiveTvReq::OpenSetup {
        bridge::dismiss_settings(&mut *t.pages);
        open_page(&mut *t.pages, &mut *t.bridge, true);
        return AppCommandOutcome::Completed;
    }
    let MachineId::Instance(instance) = source else { return AppCommandOutcome::Refused };
    let Some(entry) = t.pages.nav.entry_of_instance(instance) else { return AppCommandOutcome::Refused };
    if t.pages.nav.input_owner() != Some(InputOwner::Entry(entry)) { return AppCommandOutcome::Refused; }
    match request {
        LiveTvReq::Tab(tab) => {
            if !t.bridge.search_tab_available(tab) || matches!(tab, HomeTab::LiveTv) { return AppCommandOutcome::Refused; }
            match tab {
                HomeTab::Home => bridge::nav_home_pill(&mut *t.pages, &mut *t.bridge, Some(ret)),
                other => bridge::nav_tab(&mut *t.pages, &mut *t.bridge, other, None, Some(ret)),
            }
        }
        LiveTvReq::Account => bridge::open_account_menu(&mut *t.pages),
        LiveTvReq::Back => bridge::nav_tab(&mut *t.pages, &mut *t.bridge, HomeTab::Home, None, Some(ret)),
        LiveTvReq::Tune { index } => {
            let lineup = Arc::clone(t.bridge.livetv_view().lineup());
            if index < lineup.len() {
                let previous = plx_media::route::live(&t.player.session).map(|l| l.index).filter(|&p| p != index);
                tune(t, lineup, index, previous, now);
            }
        }
        LiveTvReq::Step(delta) => {
            let Some(live) = plx_media::route::live(&t.player.session).cloned() else { return AppCommandOutcome::Refused };
            if let Some(next) = live.lineup.step(live.index, delta) {
                tune(t, Arc::clone(&live.lineup), next, Some(live.index), now);
            }
        }
        LiveTvReq::Previous => {
            let Some(live) = plx_media::route::live(&t.player.session).cloned() else { return AppCommandOutcome::Refused };
            if let Some(prev) = live.previous.filter(|&p| p < live.lineup.len()) {
                tune(t, Arc::clone(&live.lineup), prev, Some(live.index), now);
            }
        }
        LiveTvReq::Typed(number) => {
            let Some(live) = plx_media::route::live(&t.player.session).cloned() else { return AppCommandOutcome::Refused };
            if let Some(index) = live.lineup.index_for_typed(&number) {
                if index != live.index {
                    tune(t, Arc::clone(&live.lineup), index, Some(live.index), now);
                }
            }
        }
        LiveTvReq::Number(number) => tune_number(t, &number, now),
        LiveTvReq::Detail { sid, rk } => {
            let hit = plx_data::livetv::plexmatch::Hit { sid, rk };
            if !t.bridge.livetv_view().knows_match(&hit) { return AppCommandOutcome::Refused; }
            let arg = AppArg::Content(plx_screens::registry::ContentArg::Detail { sid: hit.sid, rk: hit.rk });
            bridge::nav_push_with_return(&mut *t.pages, arg, ret);
        }
        LiveTvReq::Retry => retry(t, now),
        LiveTvReq::OpenSetup => {}
    }
    AppCommandOutcome::Completed
}

/// Tune the channel numbered `number` in the loaded guide — OK on Home's On Now card
/// (`HomeReq::Tune`). A number the guide no longer lists (it reloaded under the card) does nothing.
pub(crate) fn tune_number(t: &mut Tune<'_, '_>, number: &str, now: u32) {
    let lineup = Arc::clone(t.bridge.livetv_view().lineup());
    let Some(index) = lineup.index_of_number(number) else { return };
    let previous = plx_media::route::live(&t.player.session).map(|l| l.index).filter(|&p| p != index);
    tune(t, lineup, index, previous, now);
}

/// Tune the playing channel again — the failure read-out's *Try again* and the banner's OK on a
/// failed tune.
pub(crate) fn retry(t: &mut Tune<'_, '_>, now: u32) {
    let Some(live) = plx_media::route::live(&t.player.session).cloned() else { return };
    t.tuner.retries = 0;
    if let Some(ch) = live.channel() {
        t.tuner.known.remove(&ch.url);
    }
    tune(t, live.lineup, live.index, live.previous, now);
}

/// Should a tune of `url` ask Tunarr for native mode first? Only a Tunarr lineup URL on an origin
/// not known to lack the route, with a Plex server signed in, and not on a re-tune after a failure
/// (`retries > 0`), which plays the stream that has no Plex resolve in it to fail.
fn native_wanted(tuner: &LiveTuner, url: &str, any_server: bool) -> bool {
    any_server
        && tuner.retries == 0
        && native::channel_of(url).is_some_and(|(origin, _)| !tuner.native_unsupported.contains(&origin))
}

fn tune(t: &mut Tune<'_, '_>, lineup: Arc<plx_data::livetv::guide::Lineup>, index: usize, previous: Option<usize>, now: u32) {
    let Some(channel) = lineup.channels.get(index) else { return };
    let url = channel.url.clone();
    let virtual_playlist = plx_data::livetv::virtual_playlist(&url).map(str::to_owned);
    plx_base::eventlog::log(&format!("livetv: tune channel={} index={index}", channel.number));
    // Step 1: stop what plays, and show the channel tuning.
    super::content::halt_preview_now(&mut t.player.session, &mut t.adapters.player);
    plx_media::route::cancel_play(&mut t.player.session);
    super::playback::close_player_overlays(&mut *t.pages);
    plx_media::player::stop_bufferfeed(&mut t.player.session, &mut t.adapters.player);
    plx_media::player::report::clear_error_trace();
    let session = LiveSession::tuning(lineup, index, previous);
    plx_media::route::set_live_tuning(&mut t.player.session, session);
    let in_player = t.route() == AppArg::Player;
    if in_player {
        bridge::send_player_message(&mut *t.pages,
            plx_screens::registry::AppMsg::PlayerStart { now, hud_ms: plx_screens::player::LIVE_BANNER_MS });
    } else {
        super::playback::enter_player(&mut *t.pages, &mut *t.bridge, super::playback::Origin::Here, None);
        t.bridge.seed_player_hud(plx_screens::player::LIVE_BANNER_MS);
    }
    plx_media::player::commit_paused(&mut t.player.session, &mut t.adapters.player, false);
    t.tuner.native_program = None;
    t.tuner.native_end_ms = None;
    t.tuner.native_recheck_at = None;
    t.tuner.native_pending = None;
    if let Some(playlist) = virtual_playlist {
        t.tuner.epoch += 1;
        t.tuner.retry_at = None;
        t.tuner.pending = None;
        tune_virtual(t, &playlist, now);
        t.note_presentation_damage(plx_machine::present::Provenance::Lifecycle);
        return;
    }
    t.tuner.epoch += 1;
    t.tuner.retry_at = None;
    let any_server = plx_plex::plex::server_ids().next().is_some();
    if native_wanted(&t.tuner, &url, any_server) {
        let (tx, rx) = mpsc::channel();
        let ask_url = url.clone();
        let learned = t.tuner.native_sources.clone();
        let spawned = plx_base::task::spawn_small("livetv-native", move || {
            let _ = tx.send(native::resolve(&crate::app::adapters::livetv_work::Registry, &ask_url, &learned));
            plx_machine::present::wake_from_worker();
        });
        if spawned {
            t.tuner.pending = None;
            t.tuner.native_pending = Some(NativePending { epoch: t.tuner.epoch, url, rx });
            return;
        }
    }
    tune_stream(t, url);
}

/// Steps 2 and 3 for a Tunarr channel's `.ts` stream: start it at once on a declaration measured
/// before, else probe it on a worker.
fn tune_stream(t: &mut Tune<'_, '_>, url: String) {
    // A channel measured before starts at once.
    if let Some(facts) = known_facts(&t.tuner.known, &url, t.tuner.retries) {
        t.tuner.pending = None;
        start(t, url, facts);
        return;
    }
    t.tuner.known.remove(&url);
    // Step 2: probe on a worker.
    let (tx, rx) = mpsc::channel();
    let probe_url = url.clone();
    let spawned = plx_base::task::spawn_small("livetv-probe", move || {
        let _ = tx.send(plx_media::live::probe(&probe_url));
        plx_machine::present::wake_from_worker();
    });
    if spawned {
        t.tuner.pending = Some(Pending { epoch: t.tuner.epoch, url, rx });
    } else {
        // No worker: play with Tunarr's default declaration rather than not at all.
        start(t, url, StreamFacts::TUNARR_DEFAULT);
    }
    t.note_presentation_damage(plx_machine::present::Provenance::Lifecycle);
}

/// Steps 2 and 3 for a virtual channel: what its timeline airs now, played from where the channel
/// is in it. A channel whose timeline is not built yet (the store is still reading its programmes)
/// is retried shortly, like a probe that failed.
fn tune_virtual(t: &mut Tune<'_, '_>, playlist: &str, now: u32) {
    let wall = plx_base::wallclock::now_ms();
    let airing = {
        let view = t.bridge.livetv_view();
        view.virtuals()
            .channel(playlist)
            .and_then(|vc| vc.schedule.as_ref())
            .and_then(|s| s.at(wall).map(|slot| (slot, s.programs()[slot.program].clone())))
    };
    let Some((slot, programme)) = airing else {
        plx_base::eventlog::log(&format!("livetv: virtual channel {playlist} has no timeline yet"));
        fail(t);
        schedule_retry(t, now);
        return;
    };
    let offset_ns = (wall - slot.start_ms).max(0) * 1_000_000;
    let (title, ctx) = virtual_titles(&programme);
    plx_base::eventlog::log(&format!(
        "livetv: virtual tune rk={} offset={}s", programme.rk, offset_ns / 1_000_000_000
    ));
    let item = Programme {
        sid: ServerId::from_raw(programme.sid),
        rk: &programme.rk,
        part: &programme.part,
        vcodec: &programme.vcodec,
        acodec: &programme.acodec,
        title: &title,
        ctx: &ctx,
        airing_start_ms: slot.start_ms,
        library_item: false,
    };
    if !play_programme(t, item, offset_ns) {
        fail(t);
        schedule_retry(t, now);
    }
}

/// A library item a channel airs, as `play_programme` plays it.
struct Programme<'a> {
    sid: ServerId,
    rk: &'a str,
    part: &'a str,
    vcodec: &'a str,
    acodec: &'a str,
    title: &'a str,
    ctx: &'a str,
    /// The local wall instant the item's position 0 aired (`LiveSession::airing_start_ms`).
    airing_start_ms: i64,
    /// Mark the session as playing a library item (native mode); a virtual channel is one already.
    library_item: bool,
}

/// Play `item` from `offset_ns` under the Live TV session being tuned — quietly
/// (`route::request_play_channel`: no PlayQueue, no timeline, no scrobble). `false` when the
/// request could not be made.
fn play_programme(t: &mut Tune<'_, '_>, item: Programme<'_>, offset_ns: i64) -> bool {
    let Some(mut session) = plx_media::route::live(&t.player.session).cloned() else { return false };
    session.facts = Some(StreamFacts::TUNARR_DEFAULT);
    session.failed = false;
    session.airing_start_ms = Some(item.airing_start_ms);
    session.library_item = item.library_item;
    plx_media::route::set_live_tuning(&mut t.player.session, session);
    let requested = plx_media::route::request_play_channel(
        &mut t.player.session,
        t.bridge.metadata_mut(),
        super::playback::start_caps(),
        item.sid,
        item.rk,
        item.part,
        item.vcodec,
        item.acodec,
        item.title,
        item.ctx,
    );
    if !requested {
        return false;
    }
    super::playback::start_playback(
        &mut t.player.session,
        &mut t.adapters.player,
        offset_ns,
        super::playback::Origin::Here,
        plx_screens::player::LIVE_BANNER_MS,
        None,
        &mut *t.pages,
        &mut *t.bridge,
    );
    if let Some(channel) = plx_media::route::live(&t.player.session).and_then(|l| l.channel()) {
        t.bridge.livetv_run(plx_data::livetv::LiveTvCmd::Remember { number: channel.number.clone() });
    }
    true
}

/// Native mode's step 3: the programme Tunarr airs, from its Plex file at the channel's point.
fn tune_native(t: &mut Tune<'_, '_>, url: String, d: native::Direct) {
    let wall = plx_base::wallclock::now_ms();
    let offset_ms = native::file_offset_at(&d.clock, d.seek_offset_ms, wall);
    plx_base::eventlog::log(&format!(
        "livetv: native tune rk={} offset={}s ends={}s skew={}ms",
        d.rk, offset_ms / 1000, d.end_offset_ms / 1000, d.clock.skew_ms
    ));
    let item = Programme {
        sid: d.sid,
        rk: &d.rk,
        part: &d.part,
        vcodec: &d.vcodec,
        acodec: &d.acodec,
        title: &d.title,
        ctx: &d.ctx,
        airing_start_ms: d.airing_start_ms(),
        library_item: true,
    };
    if play_programme(t, item, offset_ms * 1_000_000) {
        t.tuner.native_program = Some(d.program_id);
        t.tuner.native_end_ms = Some(d.end_offset_ms);
    } else {
        plx_base::eventlog::log("livetv: native request refused; playing the channel's stream");
        if let Some(mut session) = plx_media::route::live(&t.player.session).cloned() {
            session.library_item = false;
            session.airing_start_ms = None;
            session.facts = None;
            plx_media::route::set_live_tuning(&mut t.player.session, session);
        }
        tune_stream(t, url);
    }
}

/// What the landing of a native-mode question does, decided apart from the session so it can be
/// tested: play the item natively, or play the stream (and when to ask again).
#[derive(Debug, PartialEq)]
enum NativeStep {
    Native(native::Direct),
    /// Play the stream; ask Tunarr again at this local instant, if ever.
    Stream { recheck_at: Option<i64>, unsupported: bool },
}

fn native_step(plan: Plan, just_ended: Option<&str>, wall: i64) -> NativeStep {
    match plan {
        Plan::Direct(d) if just_ended == Some(d.program_id.as_str()) => {
            // Still on the programme that just ended here: its tail is Tunarr's to play.
            let left = d.end_offset_ms - native::file_offset_at(&d.clock, d.seek_offset_ms, wall);
            NativeStep::Stream { recheck_at: native::recheck_at(&Why::Flex { remaining_ms: left }, wall), unsupported: false }
        }
        Plan::Direct(d) => NativeStep::Native(d),
        Plan::Stream(why) => NativeStep::Stream { recheck_at: native::recheck_at(&why, wall), unsupported: why == Why::Unsupported },
    }
}

/// The HUD's title and context line for a virtual channel's programme: the show and the episode
/// ("S2 · E5 · Title"), or the film and its year.
fn virtual_titles(p: &plx_data::vchannel::schedule::Program) -> (String, String) {
    if p.episode && !p.show_title.is_empty() {
        let code = p.episode_code();
        let ctx = [code.as_str(), p.title.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" \u{b7} ");
        (p.show_title.clone(), ctx)
    } else {
        (p.title.clone(), if p.year > 0 { p.year.to_string() } else { String::new() })
    }
}

/// Step 3: install the channel and start the engine.
fn start(t: &mut Tune<'_, '_>, url: String, facts: StreamFacts) {
    let Some(mut session) = plx_media::route::live(&t.player.session).cloned() else { return };
    session.facts = Some(facts);
    session.failed = false;
    plx_base::eventlog::log(&format!(
        "livetv: start vcodec={} acodec={} fps={:.3}",
        facts.vcodec, facts.acodec, facts.fps
    ));
    if !plx_media::route::install_live_stream(&mut t.player.session, session, &url) {
        fail(t);
        return;
    }
    if !plx_media::player::start_bufferfeed(&mut t.player.session, &mut t.adapters.player) {
        plx_base::eventlog::log("livetv: the engine refused to start");
        fail(t);
        return;
    }
    plx_media::player::commit_paused(&mut t.player.session, &mut t.adapters.player, false);
    if let Some(channel) = plx_media::route::live(&t.player.session).and_then(|l| l.channel()) {
        t.bridge.livetv_run(plx_data::livetv::LiveTvCmd::Remember { number: channel.number.clone() });
    }
    t.note_presentation_damage(plx_machine::present::Provenance::Lifecycle);
}

fn fail(t: &mut Tune<'_, '_>) {
    if let Some(mut session) = plx_media::route::live(&t.player.session).cloned() {
        session.failed = true;
        plx_media::route::set_live_tuning(&mut t.player.session, session);
    }
    t.note_presentation_damage(plx_machine::present::Provenance::Lifecycle);
}

/// Once a frame: land the native-mode answer and the probe, and run the automatic re-tune schedule.
pub(crate) fn pump(t: &mut Tune<'_, '_>, now: u32) {
    if let Some(pending) = t.tuner.native_pending.as_ref() {
        match pending.rx.try_recv() {
            Ok((plan, learnt)) => {
                let NativePending { epoch, url, .. } = t.tuner.native_pending.take().expect("checked above");
                if let Some((source, sid)) = learnt {
                    t.tuner.native_sources.insert(source, sid);
                }
                let current = epoch == t.tuner.epoch && plx_media::route::live(&t.player.session).is_some();
                if current {
                    let ended = t.tuner.native_ended.take();
                    match native_step(plan, ended.as_deref(), plx_base::wallclock::now_ms()) {
                        NativeStep::Native(d) => tune_native(t, url, d),
                        NativeStep::Stream { recheck_at, unsupported } => {
                            if unsupported {
                                if let Some((origin, _)) = native::channel_of(&url) {
                                    plx_base::eventlog::log(&format!("livetv: {origin} has no native-playback route"));
                                    t.tuner.native_unsupported.insert(origin);
                                }
                            }
                            t.tuner.native_recheck_at = recheck_at;
                            tune_stream(t, url);
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let NativePending { epoch, url, .. } = t.tuner.native_pending.take().expect("checked above");
                if epoch == t.tuner.epoch && plx_media::route::live(&t.player.session).is_some() {
                    tune_stream(t, url);
                }
            }
        }
    }
    if let Some(pending) = t.tuner.pending.as_ref() {
        match pending.rx.try_recv() {
            Ok(result) => {
                let Pending { epoch, url, .. } = t.tuner.pending.take().expect("checked above");
                let current = epoch == t.tuner.epoch && plx_media::route::live(&t.player.session).is_some();
                if current {
                    match result {
                        Ok(facts) => {
                            t.tuner.known.insert(url.clone(), facts);
                            start(t, url, facts)
                        }
                        // A stream that answered but could not be read — the probe found no PMT in
                        // the bytes it got, say — may still play: try with the default declaration.
                        Err(ProbeError::NotPlayable(why)) => {
                            plx_base::eventlog::log(&format!("livetv: probe inconclusive ({why}); playing with the default declaration"));
                            start(t, url, StreamFacts::TUNARR_DEFAULT);
                        }
                        Err(e) => {
                            plx_base::eventlog::log(&format!("livetv: probe failed ({e})"));
                            fail(t);
                            schedule_retry(t, now);
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                t.tuner.pending = None;
                fail(t);
            }
        }
    }
    let Some(live) = plx_media::route::live(&t.player.session).cloned() else {
        t.tuner.retry_at = None;
        return;
    };
    // Native mode: the channel leaves the programme at a FILE position (Tunarr's schedule cut), live
    // or paused behind; then it is asked what is on next. A stream playing over flex is asked again
    // once the flex is over.
    if live.library_item && t.route() == AppArg::Player {
        if let Some(end_ms) = t.tuner.native_end_ms {
            if plx_media::player::seen_frame(&t.player.session).unwrap_or(false)
                && plx_media::player::playpos_ns(&t.player.session).unwrap_or(0) / 1_000_000 >= end_ms
            {
                plx_base::eventlog::log("livetv: the channel left the programme; asking Tunarr what is on");
                advance_native(t, &live, now);
                return;
            }
        }
    }
    if !live.library_item && t.route() == AppArg::Player {
        if t.tuner.native_recheck_at.is_some_and(|at| plx_base::wallclock::now_ms() >= at) {
            t.tuner.native_recheck_at = None;
            plx_base::eventlog::log("livetv: the flex is over; asking Tunarr what is on");
            plx_media::player::stop_bufferfeed(&mut t.player.session, &mut t.adapters.player);
            t.tuner.retries = 0;
            tune(t, live.lineup, live.index, live.previous, now);
            return;
        }
    }
    // A channel that has shown a picture has proven itself: its retry budget is whole again.
    if live.facts.is_some() && plx_media::player::seen_frame(&t.player.session).unwrap_or(false) {
        t.tuner.retries = 0;
    }
    // An engine that failed on a live channel is re-tuned before the read-out is left standing.
    if t.tuner.retry_at.is_none()
        && live.facts.is_some()
        && plx_appkit::player_hud::transport_hidden(&super::player_presentation::capture(&t.player.session))
    {
        schedule_retry(t, now);
    }
    if t.tuner.retry_at.is_some_and(|at| now.wrapping_sub(at) < u32::MAX / 2) {
        t.tuner.retry_at = None;
        if t.route() == AppArg::Player {
            plx_base::eventlog::log(&format!("livetv: automatic re-tune {}/{}", t.tuner.retries, AUTO_RETRIES));
            tune(t, live.lineup, live.index, live.previous, now);
        }
    }
}

fn schedule_retry(t: &mut Tune<'_, '_>, now: u32) {
    if t.tuner.retries < AUTO_RETRIES {
        t.tuner.retries += 1;
        t.tuner.retry_at = Some(now.wrapping_add(RETRY_DELAY_MS));
    }
}

/// A Tunarr channel in native mode moves on from the programme it played: what is on now is asked
/// again, and the programme is remembered so a reply still naming it does not re-tune into its end.
fn advance_native(t: &mut Tune<'_, '_>, live: &LiveSession, now: u32) {
    t.tuner.native_ended = t.tuner.native_program.take();
    t.tuner.native_end_ms = None;
    plx_media::player::stop_bufferfeed(&mut t.player.session, &mut t.adapters.player);
    t.tuner.retries = 0;
    tune(t, Arc::clone(&live.lineup), live.index, live.previous, now);
}

/// The live stream drained to its end: a channel does not end, so this is a dropped connection.
/// Re-tune it (bounded); when the budget is spent, leave the banner saying it is unavailable.
pub(crate) fn stream_ended(t: &mut Tune<'_, '_>, now: u32) {
    // A Tunarr programme played natively ended (its file is done): the channel moves on.
    if let Some(live) = plx_media::route::live(&t.player.session).cloned().filter(|l| l.library_item) {
        plx_base::eventlog::log("livetv: the programme's file ended; asking Tunarr what is on");
        advance_native(t, &live, now);
        return;
    }
    // A virtual channel's programme ended: the channel moves on to the next one, at once.
    if let Some(live) = plx_media::route::live(&t.player.session).cloned().filter(|l| l.is_virtual()) {
        plx_base::eventlog::log("livetv: the programme ended; the virtual channel moves on");
        plx_media::player::stop_bufferfeed(&mut t.player.session, &mut t.adapters.player);
        t.tuner.retries = 0;
        tune(t, live.lineup, live.index, live.previous, now);
        return;
    }
    plx_base::eventlog::log("livetv: the stream ended; re-tuning");
    plx_media::player::stop_bufferfeed(&mut t.player.session, &mut t.adapters.player);
    if t.tuner.retries < AUTO_RETRIES {
        schedule_retry(t, now);
    } else {
        fail(t);
    }
}


/// Open the Live TV page's channel studio making a channel from a show, season, collection or
/// playlist (the card menu's "Make a Channel"). `kind` is the row's catalog kind.
pub(crate) fn open_make(
    pages: &mut plx_ui::dispatch::Dispatcher<AppHost>,
    rig: &mut bridge::Bridge,
    rk: &str,
    kind: std::os::raw::c_int,
    title: &str,
) {
    use plx_data::vchannel::recipe::{Recipe, Source};
    let source = match kind {
        1 => Source::Show { rk: rk.to_owned() },
        2 => Source::Season { rk: rk.to_owned() },
        plx_data::pms::KIND_PLAYLIST => Source::Playlist { rk: rk.to_owned() },
        _ => Source::Collection { rk: rk.to_owned() },
    };
    let why = plx_os::i18n::msg::livetv_studio_made_from(title);
    let recipe = Recipe::made(source, title, &why, plx_base::wallclock::now_ms());
    bridge::nav_select_tab(pages, AppArg::LiveTv);
    rig.request_livetv_make(recipe, why);
}

/// Open the Live TV page — on its setup face when asked (Settings > Live TV).
pub(crate) fn open_page(d: &mut plx_ui::dispatch::Dispatcher<AppHost>, rig: &mut bridge::Bridge, setup: bool) {
    bridge::nav_select_tab(d, AppArg::LiveTv);
    if setup {
        rig.request_livetv_setup();
    }
}

#[cfg(test)]
mod known_facts_tests {
    use super::*;

    /// A channel measured before skips its probe — unless the tune is a re-tune after a failure,
    /// when the stream may have changed under it.
    #[test]
    fn a_measured_channel_starts_without_its_probe_until_it_fails() {
        let url = "http://192.0.2.20:8000/stream/channels/1.ts";
        let facts = StreamFacts { vcodec: "h264", acodec: "aac", fps: 29.97, raster: (1920, 1080) };
        let mut known = std::collections::HashMap::new();
        assert_eq!(known_facts(&known, url, 0), None, "never measured: probe");
        known.insert(url.to_owned(), facts);
        assert_eq!(known_facts(&known, url, 0), Some(facts));
        assert_eq!(known_facts(&known, url, 1), None, "a re-tune after a failure probes afresh");
        assert_eq!(known_facts(&known, "http://192.0.2.20:8000/stream/channels/2.ts", 0), None);
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use native::{Clock, Direct};

    fn direct(program: &str) -> Direct {
        Direct {
            sid: ServerId::from_raw(1),
            rk: "4242".into(),
            part: "/library/parts/1/2/file.mkv".into(),
            vcodec: "h264".into(),
            acodec: "aac".into(),
            title: "Film".into(),
            ctx: String::new(),
            program_id: program.into(),
            clock: Clock::measure(1_000_000, 1_000_000, 1_000_000),
            seek_offset_ms: 60_000,
            end_offset_ms: 90_000,
        }
    }

    #[test]
    fn a_plex_programme_plays_natively() {
        assert_eq!(native_step(Plan::Direct(direct("p1")), None, 1_000_000), NativeStep::Native(direct("p1")));
        assert_eq!(native_step(Plan::Direct(direct("p2")), Some("p1"), 1_000_000), NativeStep::Native(direct("p2")));
    }

    /// A programme whose file ran out before Tunarr's schedule left it is not re-tuned into its own
    /// end: the tail plays from the stream, and Tunarr is asked again when the slot is over.
    #[test]
    fn the_programme_that_just_ended_plays_its_tail_from_the_stream() {
        // 10 s after the reply, 20 s of the slot remain.
        let step = native_step(Plan::Direct(direct("p1")), Some("p1"), 1_010_000);
        assert_eq!(step, NativeStep::Stream { recheck_at: Some(1_030_000), unsupported: false });
        // A slot already over waits the floor, not zero: no tight loop.
        let late = native_step(Plan::Direct(direct("p1")), Some("p1"), 1_100_000);
        assert_eq!(late, NativeStep::Stream { recheck_at: Some(1_100_000 + native::RECHECK_MIN_MS), unsupported: false });
    }

    #[test]
    fn what_the_app_cannot_play_itself_plays_the_stream() {
        assert_eq!(
            native_step(Plan::Stream(Why::Flex { remaining_ms: 30_000 }), None, 0),
            NativeStep::Stream { recheck_at: Some(30_000), unsupported: false }
        );
        assert_eq!(native_step(Plan::Stream(Why::Unsupported), None, 0), NativeStep::Stream { recheck_at: None, unsupported: true });
        assert_eq!(native_step(Plan::Stream(Why::NotPlex), None, 0), NativeStep::Stream { recheck_at: None, unsupported: false });
        assert_eq!(native_step(Plan::Stream(Why::Failed), None, 0), NativeStep::Stream { recheck_at: None, unsupported: false });
    }

    #[test]
    fn native_mode_is_asked_for_only_where_it_can_answer() {
        let url = "http://192.0.2.20:8000/stream/channels/abc.ts";
        let mut tuner = LiveTuner::default();
        assert!(native_wanted(&tuner, url, true));
        assert!(!native_wanted(&tuner, url, false), "no Plex server signed in");
        assert!(!native_wanted(&tuner, "http://hdhr.lan:5004/auto/v4.1", true), "not a Tunarr lineup URL");
        tuner.retries = 1;
        assert!(!native_wanted(&tuner, url, true), "a re-tune after a failure plays the stream");
        tuner.retries = 0;
        tuner.native_unsupported.insert("http://192.0.2.20:8000".into());
        assert!(!native_wanted(&tuner, url, true), "an older Tunarr is asked no more");
    }
}
