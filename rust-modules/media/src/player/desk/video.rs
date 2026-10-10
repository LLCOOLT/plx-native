//! The video lane: decode on one thread, present on time from another, draw on the render thread.
//!
//! **Decoder → pacer → screen.** The decoder thread turns access units into frames and hands them
//! over a channel a few frames deep, which is what bounds memory: a 4K frame is 12 MB, and the
//! app feeds well over a second ahead. The pacer holds each frame until the clock reaches its
//! time, publishes it as the picture to show and wakes the frame loop; a frame already overtaken
//! by the clock while a newer one waits is dropped, which is how a slow decoder sheds load
//! instead of drifting behind the sound. The render thread uploads the published picture once and
//! composites it under the UI (`plx_gfx::gfx::video`) on every frame it draws.
//!
//! The first picture is published as soon as it is decoded, whatever the clock says: that is the
//! pre-roll a television shows on Load, and it is the signal the audio lane waits for before the
//! sound starts, so a slow first decode delays both rather than opening on sound over black.
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use plx_gfx::gfx::video::{levels, matrix, Look, Plane, Textures, Transfer};

use super::ffdec::{pix_fmt, Decoder, Frame};

/// Decoded frames waiting for their time. Enough to ride out a slow frame; few enough that 4K
/// stays in tens of megabytes.
const PACER_DEPTH: usize = 3;
/// A frame later than this behind the clock is dropped when a newer one is already waiting.
const LATE_NS: i64 = 30_000_000;

/// One picture ready to draw: the frame and how to read it.
pub struct Picture {
    frame: Frame,
    pts: i64,
    planes: Vec<Plane>,
    look: Look,
    /// The picture's display aspect, width over height.
    aspect: f32,
}
// SAFETY: `planes` point into `frame`, which the picture owns and which is never written again.
unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

/// What the pixel formats this path can draw are numbered in this build, looked up once.
struct Formats {
    // (format, planes' (bytes per texel, chroma shift x, chroma shift y), interleaved, wide, bits, max)
    table: Vec<(c_int, Layout)>,
}

#[derive(Clone, Copy)]
struct Layout {
    chroma_shift: (u32, u32),
    interleaved: bool,
    wide: bool,
    bits: u32,
    max: f32,
}

fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| {
        let mut table = Vec::new();
        let mut add = |name: &std::ffi::CStr, l: Layout| {
            let f = pix_fmt(name);
            if f >= 0 {
                table.push((f, l));
            }
        };
        let eight = |sx, sy, interleaved| Layout { chroma_shift: (sx, sy), interleaved, wide: false, bits: 8, max: 255.0 };
        let deep = |sx, sy, bits: u32| Layout {
            chroma_shift: (sx, sy),
            interleaved: false,
            wide: true,
            bits,
            max: ((1u32 << bits) - 1) as f32,
        };
        add(c"nv12", eight(1, 1, true));
        add(c"yuv420p", eight(1, 1, false));
        add(c"yuvj420p", eight(1, 1, false));
        add(c"yuv422p", eight(1, 0, false));
        add(c"yuvj422p", eight(1, 0, false));
        add(c"yuv444p", eight(0, 0, false));
        add(c"yuvj444p", eight(0, 0, false));
        add(c"yuv420p10le", deep(1, 1, 10));
        add(c"yuv422p10le", deep(1, 0, 10));
        add(c"yuv444p10le", deep(0, 0, 10));
        add(c"yuv420p12le", deep(1, 1, 12));
        // P010: 10-bit codes in the TOP of each 16-bit word, chroma interleaved.
        add(c"p010le", Layout { chroma_shift: (1, 1), interleaved: true, wide: true, bits: 16, max: 65535.0 });
        Formats { table }
    })
}

fn picture(frame: Frame) -> Option<Picture> {
    let pts = frame.pts()?;
    let format = frame.format();
    let Some(&(_, l)) = formats().table.iter().find(|(f, _)| *f == format) else {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Relaxed) {
            plx_base::eventlog::log(&format!("desk: pixel format {format} is not drawable; no picture"));
        }
        return None;
    };
    let (w, h) = (frame.width(), frame.height());
    if w <= 0 || h <= 0 {
        return None;
    }
    let word = if l.wide { 2 } else { 1 };
    let (cw, ch) = ((w + (1 << l.chroma_shift.0) - 1) >> l.chroma_shift.0, (h + (1 << l.chroma_shift.1) - 1) >> l.chroma_shift.1);
    let plane = |i: usize, pw, ph, bytes| {
        let (data, stride) = frame.plane(i);
        Plane { data, stride, w: pw, h: ph, bytes }
    };
    let planes = if l.interleaved {
        vec![plane(0, w, h, word), plane(1, cw, ch, 2 * word)]
    } else {
        vec![plane(0, w, h, word), plane(1, cw, ch, word), plane(2, cw, ch, word)]
    };
    if planes.iter().any(|p| p.data.is_null() || p.stride < p.w * p.bytes) {
        return None;
    }
    let transfer = match frame.color_trc() {
        16 => Transfer::Pq,
        18 => Transfer::Hlg,
        _ => Transfer::Sdr,
    };
    // AVCOL_RANGE_JPEG (2) is full range; unspecified is limited, as broadcast and every disc is.
    let full = frame.color_range() == 2;
    let look = Look {
        interleaved: l.interleaved,
        wide: l.wide,
        max: l.max,
        levels: levels(full, l.bits, l.max),
        k: matrix(frame.colorspace(), h),
        transfer,
    };
    let (sn, sd) = frame.sar();
    let sar = if sn > 0 && sd > 0 { sn as f32 / sd as f32 } else { 1.0 };
    Some(Picture { aspect: w as f32 * sar / h as f32, pts, planes, look, frame })
}

/// The picture on screen now, with a serial the render thread's upload is keyed on.
static LATEST: Mutex<Option<(u64, Arc<Picture>)>> = Mutex::new(None);
static SERIAL: AtomicU64 = AtomicU64::new(0);
/// Set when a session publishes its first picture; read by the session to start the sound and the
/// position reports.
pub static SHOWN: AtomicBool = AtomicBool::new(false);

/// Forget the picture on screen (the player stopped).
pub fn clear() {
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
    SHOWN.store(false, Relaxed);
    plx_machine::idle::invalidate();
}

fn publish(p: Arc<Picture>) {
    let n = SERIAL.fetch_add(1, Relaxed) + 1;
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some((n, p));
    SHOWN.store(true, Relaxed);
    plx_machine::idle::invalidate();
}

/// One session's video lane.
pub struct Lane {
    inbox: Arc<(Mutex<Inbox>, Condvar)>,
    stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inbox {
    aus: VecDeque<(Vec<u8>, i64)>,
    eos: bool,
}

impl Lane {
    /// Start decoding `codec` (`h264`, `hevc`) on `hw` when named, presenting against `clock`.
    pub fn start(codec: &'static str, hw: Option<&'static str>, clock: fn() -> Option<i64>) -> Option<Lane> {
        let inbox = Arc::new((Mutex::new(Inbox::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel::<Picture>(PACER_DEPTH);
        let (i, s) = (inbox.clone(), stop.clone());
        std::thread::Builder::new().name("desk-vdec".into()).spawn(move || decode(codec, hw, &i, &s, tx)).ok()?;
        let s = stop.clone();
        std::thread::Builder::new().name("desk-vpace".into()).spawn(move || pace(rx, &s, clock)).ok()?;
        Some(Lane { inbox, stop })
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
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.inbox.1.notify_all();
    }
}

fn decode(codec: &'static str, hw: Option<&'static str>, inbox: &(Mutex<Inbox>, Condvar), stop: &AtomicBool, tx: SyncSender<Picture>) {
    let Some(mut dec) = Decoder::open(codec, hw) else { return };
    plx_base::eventlog::log(&format!("desk: video {codec} decoding on {}", dec.engine));
    let mut first = true;
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
        // `send` refuses with EAGAIN only while frames wait to be received; receive and retry.
        loop {
            let rc = match &next {
                Some((au, pts)) => dec.send(Some((au, *pts))),
                None => dec.send(None),
            };
            let again = matches!(rc, Err(e) if Decoder::is_again(e));
            while let Some(frame) = dec.receive() {
                if let Some(p) = picture(frame) {
                    if first {
                        first = false;
                        plx_base::eventlog::log(&format!(
                            "desk: first picture {}x{} {:?} at {} ms",
                            p.frame.width(),
                            p.frame.height(),
                            p.look.transfer,
                            p.pts / 1_000_000
                        ));
                    }
                    // Blocks while the pacer is full: the backpressure that bounds memory.
                    if tx.send(p).is_err() || stop.load(Relaxed) {
                        return;
                    }
                }
            }
            if !again {
                break;
            }
        }
    }
}

fn pace(rx: Receiver<Picture>, stop: &AtomicBool, clock: fn() -> Option<i64>) {
    let mut waiting: Option<Picture> = None;
    let mut shown_any = false;
    loop {
        if stop.load(Relaxed) {
            return;
        }
        let p = match waiting.take() {
            Some(p) => p,
            None => match rx.recv() {
                Ok(p) => p,
                Err(_) => return,
            },
        };
        if !shown_any {
            shown_any = true;
            if stop.load(Relaxed) {
                return;
            }
            publish(Arc::new(p));
            super::refresh_sound();
            continue;
        }
        let now = clock();
        match now {
            Some(now) if p.pts <= now => {
                // Due. If it is already late and a newer frame is ready, skip to that one.
                if now - p.pts > LATE_NS {
                    match rx.try_recv() {
                        Ok(newer) => {
                            waiting = Some(newer);
                            continue;
                        }
                        Err(TryRecvError::Disconnected) => return,
                        Err(TryRecvError::Empty) => {}
                    }
                }
                if stop.load(Relaxed) {
                    return;
                }
                publish(Arc::new(p));
            }
            _ => {
                // Not due yet, or no clock running: look again shortly. Paused costs a wake-up
                // every few milliseconds and nothing else.
                let wait = now.map_or(10_000_000, |now| (p.pts - now).clamp(1_000_000, 10_000_000));
                std::thread::sleep(std::time::Duration::from_nanos(wait as u64));
                waiting = Some(p);
            }
        }
    }
}

/// The render thread's half: upload the published picture when it changed and composite it under
/// what the UI drew this frame, fitted to the canvas by its display aspect. Called once per drawn
/// player frame, before the capture and the swap.
pub fn composite_under() {
    thread_local! {
        static TEX: std::cell::RefCell<(Textures, u64)> = std::cell::RefCell::new((Textures::default(), 0));
    }
    let latest = LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    TEX.with(|t| {
        let mut t = t.borrow_mut();
        let Some((n, p)) = latest else {
            if t.1 != 0 {
                t.0.release();
                t.1 = 0;
            }
            return;
        };
        if t.1 != n {
            // SAFETY: the planes point into `p.frame`, alive for as long as `p` is.
            unsafe { t.0.upload(&p.planes) };
            t.1 = n;
        }
        t.0.draw_under(&p.look, fit(p.aspect));
    });
}

/// The largest rectangle of `aspect` centred in the 1920x1080 canvas.
fn fit(aspect: f32) -> [f32; 4] {
    let (cw, ch) = (plx_base::surface::LOGICAL_W, plx_base::surface::LOGICAL_H);
    if !(aspect.is_finite() && aspect > 0.0) {
        return [0.0, 0.0, cw, ch];
    }
    let (w, h) = if aspect > cw / ch { (cw, cw / aspect) } else { (ch * aspect, ch) };
    [(cw - w) / 2.0, (ch - h) / 2.0, w, h]
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn a_picture_is_fitted_and_centred_in_the_canvas() {
        let near = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);
        assert!(near(fit(16.0 / 9.0), [0.0, 0.0, 1920.0, 1080.0]));
        assert!(near(fit(2.4), [0.0, 140.0, 1920.0, 800.0]));
        assert!(near(fit(4.0 / 3.0), [240.0, 0.0, 1440.0, 1080.0]));
        assert!(near(fit(0.0), [0.0, 0.0, 1920.0, 1080.0]));
    }
}
