//! The audio lane, and the clock the desktop plays to.
//!
//! **Audio is the master clock**, as in every desktop player: a sound card consumes samples at its
//! own crystal's rate and cannot be told to wait, while a picture can be shown a little early or
//! late without anyone noticing. So the position the sink reports, the one the app's feed-ahead
//! throttle, HUD and resume points all read, is the presentation time of the sample the card is
//! playing now: the end of what was queued, less what is still waiting in the queue and the
//! device's own buffer. A starved queue therefore stops the clock, which is the shape a television
//! pipeline's underrun has too, and a paused device stops it without a word from anyone.
//!
//! The OUTPUT is the port's ([`AudioOut`]): this crate links no audio library, and its test
//! binaries must not either. The desktop port installs an SDL implementation at start-up; with
//! none installed (the host tests, a machine with no sound device) the lane still decodes, the
//! session runs on the wall clock, and the log says why it is silent.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use super::ffdec::{Decoder, Resampler};

/// Where decoded sound goes: interleaved 32-bit float, at a rate and channel count the device
/// agreed to. Implemented by the desktop port over SDL; every method may be called from any
/// thread.
pub trait AudioOut: Sync {
    /// Open the device (or re-open it with a different shape) for about `rate` Hz and `channels`
    /// channels, paused. Answers what it granted, which the samples must then match.
    fn open(&self, rate: i32, channels: i32) -> Option<(i32, i32)>;
    fn queue(&self, samples: &[f32]);
    /// Bytes queued and not yet handed to the device.
    fn queued_bytes(&self) -> u32;
    /// The device's own buffer, in sample frames: played after the queue, so behind the clock.
    fn device_frames(&self) -> u32;
    fn clear(&self);
    fn set_paused(&self, paused: bool);
}

static OUT: OnceLock<&'static dyn AudioOut> = OnceLock::new();

/// The port's audio output. Once, at start-up.
pub fn install(out: &'static dyn AudioOut) {
    let _ = OUT.set(out);
}

fn out() -> Option<&'static dyn AudioOut> {
    OUT.get().copied()
}

/// Run the card, or hold it (pause, or no picture up yet). Holding keeps the queue, so the clock
/// stops exactly where the sound did.
pub fn set_running(run: bool) {
    if let Some(o) = out() {
        o.set_paused(!run);
    }
}

/// What the clock is derived from. Written by the lane as it queues, read by the clock.
#[derive(Default)]
struct Queued {
    /// The presentation time of the end of everything queued, fed-PTS nanoseconds.
    end_ns: i64,
    /// Nanoseconds of sound per queued byte, for the granted shape.
    ns_per_byte: f64,
    /// The device's buffer, in nanoseconds.
    device_ns: i64,
    /// The first sample's time, the floor the clock never reads below.
    first_ns: i64,
    started: bool,
}

/// How far ahead of the card the lane decodes before it waits: comfortably past the app's own
/// feed-ahead budget, so it is never the lane that throttles the feeder.
const QUEUE_AHEAD_NS: i64 = 4_000_000_000;

/// One session's audio lane: the access units waiting to be decoded, and the queue's clock.
pub struct Lane {
    inbox: Arc<(Mutex<Inbox>, Condvar)>,
    queued: Arc<Mutex<Queued>>,
    stop: Arc<AtomicBool>,
    /// No sound can be played this session (no output, or the device would not open): the
    /// session falls back to the wall clock rather than waiting on a card that will never run.
    silent: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inbox {
    aus: VecDeque<(Vec<u8>, i64)>,
    eos: bool,
}

impl Lane {
    /// Start decoding `codec` (an FFmpeg decoder name) on its own thread.
    pub fn start(codec: &'static str) -> Option<Lane> {
        let inbox = Arc::new((Mutex::new(Inbox::default()), Condvar::new()));
        let queued = Arc::new(Mutex::new(Queued::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let silent = Arc::new(AtomicBool::new(out().is_none()));
        if out().is_none() {
            plx_base::eventlog::log("desk: no audio output installed; this session is silent");
        }
        let (i, q, s, m) = (inbox.clone(), queued.clone(), stop.clone(), silent.clone());
        std::thread::Builder::new()
            .name("desk-audio".into())
            .spawn(move || run(codec, &i, &q, &s, &m))
            .ok()?;
        Some(Lane { inbox, queued, stop, silent })
    }

    pub fn feed(&self, au: &[u8], pts: i64) {
        let (m, cv) = &*self.inbox;
        m.lock().unwrap_or_else(|e| e.into_inner()).aus.push_back((au.to_vec(), pts));
        cv.notify_one();
    }

    pub fn end_of_stream(&self) {
        let (m, cv) = &*self.inbox;
        m.lock().unwrap_or_else(|e| e.into_inner()).eos = true;
        cv.notify_one();
    }

    /// Is this session's sound unplayable, so its clock is the wall's?
    pub fn silent(&self) -> bool {
        self.silent.load(Relaxed)
    }

    /// The card's position, once the first sample is queued.
    pub fn position_ns(&self) -> Option<i64> {
        let q = self.queued.lock().unwrap_or_else(|e| e.into_inner());
        if !q.started {
            return None;
        }
        let waiting = out().map_or(0, |o| o.queued_bytes()) as f64 * q.ns_per_byte;
        Some((q.end_ns - waiting as i64 - q.device_ns).max(q.first_ns))
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.inbox.1.notify_all();
        if let Some(o) = out() {
            o.set_paused(true);
            o.clear();
        }
    }
}

fn run(codec: &'static str, inbox: &(Mutex<Inbox>, Condvar), queued: &Mutex<Queued>, stop: &AtomicBool, silent: &AtomicBool) {
    let Some(mut dec) = Decoder::open(codec, None) else {
        silent.store(true, Relaxed);
        return;
    };
    let mut resampler: Option<Resampler> = None;
    loop {
        let next = {
            let (m, cv) = inbox;
            let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if stop.load(Relaxed) {
                    return;
                }
                if let Some(au) = g.aus.pop_front() {
                    break Some(au);
                }
                if g.eos {
                    g.eos = false;
                    break None;
                }
                g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        let sent = match &next {
            Some((au, pts)) => dec.send(Some((au, *pts))),
            None => dec.send(None),
        };
        if let Err(rc) = sent {
            if !Decoder::is_again(rc) {
                continue; // one bad frame is a click, not the end of the sound
            }
        }
        while let Some(frame) = dec.receive() {
            if stop.load(Relaxed) {
                return;
            }
            let Some(pts) = frame.pts() else { continue };
            let Some(o) = out().filter(|_| !silent.load(Relaxed)) else { continue };
            if resampler.is_none() {
                // Ask for the stream's own surround layout and take what the device grants.
                let want = match frame.channels() {
                    c if c >= 8 => 8,
                    c if c >= 6 => 6,
                    _ => 2,
                };
                let Some((rate, channels)) = o.open(48_000, want) else {
                    silent.store(true, Relaxed);
                    plx_base::eventlog::log("desk: the audio device did not open; this session is silent");
                    continue;
                };
                plx_base::eventlog::log(&format!(
                    "desk: audio {codec} {}ch {}Hz -> device {channels}ch {rate}Hz",
                    frame.channels(),
                    frame.sample_rate()
                ));
                let mut q = queued.lock().unwrap_or_else(|e| e.into_inner());
                q.ns_per_byte = 1e9 / (rate as f64 * channels as f64 * 4.0);
                q.device_ns = o.device_frames() as i64 * 1_000_000_000 / rate.max(1) as i64;
                drop(q);
                resampler = Some(Resampler::new(rate, channels));
                // The device opens held; run it now if the session already should be running.
                super::refresh_sound();
            }
            let Some(r) = resampler.as_mut() else { continue };
            let mut samples = r.convert(&frame);
            if samples.is_empty() {
                continue;
            }
            // Wait, rather than decode further ahead, while the card is far enough behind.
            loop {
                if stop.load(Relaxed) {
                    return;
                }
                let ahead = {
                    let q = queued.lock().unwrap_or_else(|e| e.into_inner());
                    (o.queued_bytes() as f64 * q.ns_per_byte) as i64
                };
                if ahead < QUEUE_AHEAD_NS {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let mut q = queued.lock().unwrap_or_else(|e| e.into_inner());
            let duration = (samples.len() as f64 * 4.0 * q.ns_per_byte) as i64;
            if !q.started {
                q.started = true;
                q.first_ns = pts;
                q.end_ns = pts;
            }
            // A gap in the stream's timestamps is a gap in the sound: the clock jumps with it.
            // Overlap (a resampler's delay, rounding) is absorbed into the running end instead.
            if pts > q.end_ns + 100_000_000 {
                q.end_ns = pts;
            }
            q.end_ns += duration;
            // The desktop player's volume (`player::volume`): applied here, at the one place
            // samples leave for the card, so the clock above never sees it.
            let gain = super::super::volume::gain();
            if gain != 1.0 {
                for s in samples.iter_mut() {
                    *s *= gain;
                }
            }
            o.queue(&samples);
        }
    }
}
