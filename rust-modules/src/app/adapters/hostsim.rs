//! The simulator's side of the playback seam: the host sink the desktop platform's `SERVICES` holds, and the decoded
//! picture that sink is built with. The sink itself is the Feed backend's stand-in
//! (`plx_play_feed::ffi_host`); what lives here is what only the application may name, because it
//! draws: `sim_video` composites the picture under the finished UI frame with `plx_gfx`, and `desk`
//! (the desktop's own decoder, over the FFmpeg a desktop app ships with decoders) answers the sink's
//! verbs with a real picture and real sound. The media layer's host tests build their own sink
//! beside it (`player::test_physical`, with no picture).
//!
//! `hostsim` builds only: the television's port holds the Starfish sink.

pub(crate) mod desk;
mod sim_video;

use plx_play_feed::ffi_host::{HostSink, Hooks};

/// The desktop decoder's audio output seam: the desktop platform installs its SDL output once, at
/// start-up (`desk::audio::install`). Without one the decoder still decodes and runs on the wall
/// clock, silently.
pub use desk::audio::{install as install_desk_audio, AudioOut as DeskAudioOut, SDL_OUTPUT as DESK_SDL_AUDIO};

/// The simulator's video sink, built with the simulator's decoded picture. The desktop platform
/// (`plx_platform_desktop`, its `src/lib.rs`) holds it as `Services.legacy_webos_media.sink`; a hostsim test binary of this crate reads it directly.
/// `pub` for `host`, which re-exports it (under this module's own cfg) until W11a moves the composition.
pub static HOST_SINK: HostSink = HostSink::new(Hooks::over(&sim_video::SimVideo).decoding(&desk::DESK));

/// The render thread's half of the simulator's picture, once per bound-plane frame, after the
/// straighten pass and before the capture and the swap: the current decoded frame, composited UNDER
/// what the UI drew through the television compositor's straight-alpha mix. `false` when there is no
/// picture to put under it (not armed, or nothing decoded yet), which is the caller's cue to
/// composite over black instead.
pub(crate) fn composite_under() -> bool {
    // Either picture source, never both at once: `plxnative-simvideo` arms the screenshot picture
    // and the desktop decoder answers only when the clock sink is not armed.
    let sim = sim_video::composite_under();
    desk::video::composite_under() || sim
}

/// The simulator's clock-sink stop; the television's pipeline has no such control.
pub(crate) fn stop_sim_clock_at(media_ns: Option<i64>) -> bool {
    HOST_SINK.stop_clock_at(media_ns)
}
