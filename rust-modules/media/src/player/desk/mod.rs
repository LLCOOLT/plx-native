//! **The desktop's own decoder**, behind the simulator's sink (`player::ffi_host`).
//!
//! A television decodes in hardware the engine never sees: the pump feeds access units to
//! Starfish and the pipeline reports back a position. A desktop has no such pipeline, so this is
//! one, shaped to answer the sink's verbs the way the television's does. Each Load starts a
//! session of two lanes, video ([`video`]) and audio ([`audio`]), fed the same Annex-B and audio
//! frames Starfish would have been fed, decoded by the bundled FFmpeg's desktop half
//! ([`ffdec`]), on the OS's hardware decoder where it has one (VideoToolbox on macOS, D3D11VA on
//! Windows) and in software otherwise. The sound card's position is the clock; a session with no
//! audio runs on the sink's wall clock instead.
//!
//! **What stays the television's.** Everything upstream of the sink — the PMS decision, the
//! transports, the demuxer, the AU queues, the feed-ahead throttle, seek (a fresh Load on this
//! `VP_EXPORTED` path), the HUD — is the same code a television runs; only the verbs underneath
//! are answered here. So a desktop that plays a file says nothing about whether LG's decoder will,
//! which is why the heartbeat still carries `sim=1`.
//!
//! Chosen over the clock sink whenever the decode half loaded, unless a session armed
//! `plxnative-clocksink` (the pipeline experiment wants nothing decoded) or `plxnative-simvideo`
//! (the screenshot picture). Never in the host tests: they drive the sink's own clock.
pub mod audio;
mod ffdec;
pub mod video;

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// Decode on this desktop? Latched at the first Load.
pub fn available() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    {
        false
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        ffdec::available()
    }
}

/// The codecs one Load declared, in FFmpeg's decoder names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Codecs {
    video: Option<&'static str>,
    audio: Option<&'static str>,
}

/// The Load payload's codec declaration (`engine::payload`): `"video":"H264"|"H265"`,
/// `"audio":"AC3"|"AC3 PLUS"|"AAC"|"DTS"`, audio only when `"needAudio":true`.
fn codecs(payload: &str) -> Codecs {
    let value = |key: &str| {
        let at = payload.find(&format!("\"{key}\":\""))? + key.len() + 4;
        let end = payload[at..].find('"')?;
        Some(&payload[at..at + end])
    };
    let video = match value("video") {
        Some("H265") => Some("hevc"),
        Some("H264") => Some("h264"),
        _ => None,
    };
    let audio = if payload.contains("\"needAudio\":true") {
        match value("audio") {
            Some("AC3") => Some("ac3"),
            Some("AC3 PLUS") => Some("eac3"),
            Some("AAC") => Some("aac"),
            Some("DTS") => Some("dca"),
            _ => None,
        }
    } else {
        None
    };
    Codecs { video, audio }
}

/// The OS's hardware decoder, by FFmpeg's device name. `plxnative-hwdec=<name>` overrides it, and
/// `plxnative-hwdec=none` decodes in software.
fn hardware() -> Option<&'static str> {
    static HW: OnceLock<Option<&'static str>> = OnceLock::new();
    *HW.get_or_init(|| {
        if let Some(v) = plx_base::devtrig::read("hwdec") {
            let v = v.trim();
            return (!v.is_empty() && v != "none").then(|| &*Box::leak(v.to_string().into_boxed_str()));
        }
        if cfg!(target_os = "macos") {
            Some("videotoolbox")
        } else if cfg!(windows) {
            Some("d3d11va")
        } else {
            None
        }
    })
}

struct Session {
    codecs: Codecs,
    video: Option<video::Lane>,
    audio: Option<audio::Lane>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
/// The app asked for playback (Play since the last Pause or Load).
static PLAYING: AtomicBool = AtomicBool::new(false);
/// The sink's wall clock, for a session without sound.
static WALL: OnceLock<fn() -> i64> = OnceLock::new();

fn session() -> std::sync::MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
}

/// A new stream: drop the last one's lanes and start this one's. `wall` is the sink's own clock.
pub fn load(payload: &str, wall: fn() -> i64) {
    let _ = WALL.set(wall);
    start(codecs(payload));
}

fn start(codecs: Codecs) {
    let mut s = session();
    *s = None; // the old lanes stop, and the sound is cleared, before the new ones exist
    video::clear();
    PLAYING.store(false, Relaxed);
    plx_base::eventlog::log(&format!("desk: session video={:?} audio={:?}", codecs.video, codecs.audio));
    *s = Some(Session {
        codecs,
        video: codecs.video.and_then(|c| video::Lane::start(c, hardware(), pacer_clock)),
        audio: codecs.audio.and_then(audio::Lane::start),
    });
}

/// One access unit, Annex-B video (`es_data == 1`) or an audio frame.
pub fn feed(au: &[u8], pts: i64, video: bool) {
    let s = session();
    let Some(s) = s.as_ref() else { return };
    if video {
        if let Some(l) = &s.video {
            l.feed(au, pts);
        }
    } else if let Some(l) = &s.audio {
        l.feed(au, pts);
    }
}

pub fn end_of_stream() {
    if let Some(s) = session().as_ref() {
        if let Some(l) = &s.video {
            l.end_of_stream();
        }
        if let Some(l) = &s.audio {
            l.end_of_stream();
        }
    }
}

pub fn play() {
    PLAYING.store(true, Relaxed);
    refresh_sound();
}

pub fn pause() {
    PLAYING.store(false, Relaxed);
    refresh_sound();
}

/// A flush restarts the decoders on the same codecs (the fed timeline restarts with it).
pub fn flush() {
    let codecs = session().as_ref().map(|s| s.codecs);
    if let Some(codecs) = codecs {
        let playing = PLAYING.load(Relaxed);
        start(codecs);
        PLAYING.store(playing, Relaxed);
    }
}

/// Unload: no stream, no picture, no sound.
pub fn stop() {
    *session() = None;
    video::clear();
    PLAYING.store(false, Relaxed);
}

/// The sound runs while the app plays AND the first picture is up (or there is no picture to
/// wait for), so a session opens on picture and sound together.
pub(crate) fn refresh_sound() {
    let s = session();
    let Some(s) = s.as_ref() else { return };
    if s.audio.as_ref().is_none_or(|a| a.silent()) {
        return;
    }
    let run = PLAYING.load(Relaxed) && (s.video.is_none() || video::SHOWN.load(Relaxed));
    audio::set_running(run);
}

/// The session's position when it has sound: the card's, or before the first sample, `None`.
fn sound_clock() -> Option<Option<i64>> {
    let s = session();
    let s = s.as_ref()?;
    let a = s.audio.as_ref().filter(|a| !a.silent())?;
    Some(a.position_ns())
}

/// What the pacer presents against: the card when there is sound, else the sink's wall clock.
fn pacer_clock() -> Option<i64> {
    match sound_clock() {
        Some(sound) => sound,
        None => WALL.get().map(|f| f()),
    }
}

/// The position the sink reports, when this session has one of its own (it has sound). `None`
/// leaves the sink on its wall clock.
pub fn position_ns() -> Option<i64> {
    sound_clock().flatten()
}

/// Has this session put a picture up? Until it has, the sink reports no position, as a
/// television's pipeline reports none before its first frame.
pub fn shown() -> bool {
    video::SHOWN.load(Relaxed) || session().as_ref().is_some_and(|s| s.video.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_load_declaration_names_the_decoders() {
        let av = r#"{"codec":{"video":"H265","audio":"AC3 PLUS"},"needAudio":true}"#;
        assert_eq!(codecs(av), Codecs { video: Some("hevc"), audio: Some("eac3") });
        let v = r#"{"codec":{"video":"H264"},"needAudio":false}"#;
        assert_eq!(codecs(v), Codecs { video: Some("h264"), audio: None });
        let dts = r#"{"codec":{"video":"H264","audio":"DTS"},"needAudio":true}"#;
        assert_eq!(codecs(dts).audio, Some("dca"));
        let aac = r#"{"codec":{"video":"H264","audio":"AAC"},"needAudio":true}"#;
        assert_eq!(codecs(aac).audio, Some("aac"));
    }
}
