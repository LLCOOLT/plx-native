//! The decode half of the bundled FFmpeg, which only the desktop build carries
//! (`HOST=1 ci/build-ffmpeg.sh` adds the decoders and libswresample; the television's FFmpeg is
//! demux-only and `ff.rs` never decodes a picture there).
//!
//! Its own `dynlib!` tables rather than more rows in `ff.rs`'s: those tables are all-or-nothing, so
//! a decode entry point there would make the television's demuxer refuse to load for want of a
//! symbol it never calls. Here a missing decoder costs exactly the desktop's playback, and says so.
//!
//! **The ABI rule is `ff.rs`'s**: every function is called by name and every struct the app does
//! not allocate is reached through a library call, except the few `AVFrame` and `AVCodecContext`
//! fields read or written at an offset below, which `ci/ffabi-assert.c` proves against the bundled
//! headers ("desktop decode" block). The desktop build is 64-bit only, so the table is too.
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::OnceLock;

use crate::ff::AVPacket;

#[cfg(not(target_pointer_width = "64"))]
compile_error!("the desktop decode path has an AVFrame table for 64-bit pointers only");

plx_base::dynlib! {
    dec_avutil: [
        "libavutil-plx.so.61",
        "libavutil-plx.61.dylib",
        "avutil-plx-61.dll",
    ] {
    fn av_frame_alloc() -> *mut c_void;
    fn av_frame_free(frame: *mut *mut c_void);
    fn av_frame_unref(frame: *mut c_void);
    fn av_frame_move_ref(dst: *mut c_void, src: *mut c_void);
    fn av_frame_copy_props(dst: *mut c_void, src: *const c_void) -> c_int;
    fn av_hwdevice_find_type_by_name(name: *const c_char) -> c_int;
    fn av_hwdevice_ctx_create(
        device_ctx: *mut *mut c_void,
        type_: c_int,
        device: *const c_char,
        opts: *mut c_void,
        flags: c_int,
    ) -> c_int;
    fn av_hwframe_transfer_data(dst: *mut c_void, src: *const c_void, flags: c_int) -> c_int;
    fn av_buffer_unref(buf: *mut *mut c_void);
    fn av_get_pix_fmt(name: *const c_char) -> c_int;
    fn av_get_sample_fmt(name: *const c_char) -> c_int;
    fn av_opt_set(obj: *mut c_void, name: *const c_char, val: *const c_char, search_flags: c_int) -> c_int;
    fn av_channel_layout_default(layout: *mut c_void, nb_channels: c_int);
    fn av_channel_layout_uninit(layout: *mut c_void);
}}
plx_base::dynlib! {
    dec_swresample: [
        "libswresample-plx.so.7",
        "libswresample-plx.7.dylib",
        "swresample-plx-7.dll",
    ] {
    fn swr_alloc_set_opts2(
        s: *mut *mut c_void,
        out_layout: *const c_void,
        out_fmt: c_int,
        out_rate: c_int,
        in_layout: *const c_void,
        in_fmt: c_int,
        in_rate: c_int,
        log_offset: c_int,
        log_ctx: *mut c_void,
    ) -> c_int;
    fn swr_init(s: *mut c_void) -> c_int;
    fn swr_get_out_samples(s: *mut c_void, in_samples: c_int) -> c_int;
    fn swr_convert(
        s: *mut c_void,
        out: *const *mut u8,
        out_count: c_int,
        input: *const *const u8,
        in_count: c_int,
    ) -> c_int;
    fn swr_free(s: *mut *mut c_void);
}}
plx_base::dynlib! {
    dec_avcodec: [
        "libavcodec-plx.so.63",
        "libavcodec-plx.63.dylib",
        "avcodec-plx-63.dll",
    ] {
    fn avcodec_find_decoder_by_name(name: *const c_char) -> *const c_void;
    fn avcodec_alloc_context3(codec: *const c_void) -> *mut c_void;
    fn avcodec_open2(ctx: *mut c_void, codec: *const c_void, opts: *mut c_void) -> c_int;
    fn avcodec_free_context(ctx: *mut *mut c_void);
    fn avcodec_send_packet(ctx: *mut c_void, pkt: *const AVPacket) -> c_int;
    fn avcodec_receive_frame(ctx: *mut c_void, frame: *mut c_void) -> c_int;
    fn av_packet_alloc() -> *mut AVPacket;
    fn av_packet_free(pkt: *mut *mut AVPacket);
}}

// AVFrame (libavutil 61, 64-bit). `ci/ffabi-assert.c`, "desktop decode".
const OFF_FRAME_DATA: usize = 0;
const OFF_FRAME_LINESIZE: usize = 64;
const OFF_FRAME_EXTENDED_DATA: usize = 96;
const OFF_FRAME_WIDTH: usize = 104;
const OFF_FRAME_HEIGHT: usize = 108;
const OFF_FRAME_NB_SAMPLES: usize = 112;
const OFF_FRAME_FORMAT: usize = 116;
const OFF_FRAME_SAR: usize = 124;
const OFF_FRAME_PTS: usize = 136;
const OFF_FRAME_SAMPLE_RATE: usize = 180;
const OFF_FRAME_COLOR_RANGE: usize = 280;
const OFF_FRAME_COLOR_TRC: usize = 288;
const OFF_FRAME_COLORSPACE: usize = 292;
const OFF_FRAME_BEST_EFFORT_TS: usize = 304;
const OFF_FRAME_CH_LAYOUT: usize = 384;
/// `AVCodecContext.hw_device_ctx` — the one field with no AVOption, so the one write by offset.
const OFF_CTX_HW_DEVICE_CTX: usize = 560;
/// `sizeof(AVChannelLayout)` is 24; the buffer is generous and 8-aligned.
const CH_LAYOUT_BYTES: usize = 64;

const AV_NOPTS_VALUE: i64 = i64::MIN;
fn eagain() -> c_int {
    -libc::EAGAIN
}

/// Did the decode half load, and does it carry the decoders the payload vocabulary names? Asked
/// once; logged once. The host tests never ask (`desk::available` answers false there).
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
pub fn available() -> bool {
    static ONCE: OnceLock<bool> = OnceLock::new();
    *ONCE.get_or_init(|| {
        let dir = Some(plx_base::paths::app_dir());
        let mut ok = true;
        // Dependency order, as `ff::load_libraries` explains: libavutil first, then what needs it.
        for (what, verdict) in [
            ("avutil", dec_avutil::load(dir)),
            ("swresample", dec_swresample::load(dir)),
            ("avcodec", dec_avcodec::load(dir)),
        ] {
            match verdict {
                plx_base::dynlib::Loaded::Ok(_) => {}
                plx_base::dynlib::Loaded::NoLibrary => {
                    ok = false;
                    plx_base::eventlog::log(&format!("desk: no {what} beside the app — this build cannot decode"));
                }
                plx_base::dynlib::Loaded::Incomplete(soname, n) => {
                    ok = false;
                    plx_base::eventlog::log(&format!(
                        "desk: {soname} lacks {n} decode symbol(s) — a television FFmpeg, not the desktop's"
                    ));
                }
            }
        }
        if ok {
            let missing: Vec<&str> = ["h264", "hevc", "ac3", "eac3", "aac", "dca"]
                .into_iter()
                .filter(|name| find(name).is_null())
                .collect();
            if !missing.is_empty() {
                plx_base::eventlog::log(&format!("desk: the bundled FFmpeg has no {missing:?} decoder"));
                ok = !missing.contains(&"h264");
            }
        }
        plx_base::eventlog::log(&format!("desk: decode {}", if ok { "available" } else { "UNAVAILABLE" }));
        ok
    })
}

fn find(name: &str) -> *const c_void {
    let Ok(c) = std::ffi::CString::new(name) else { return std::ptr::null() };
    unsafe { avcodec_find_decoder_by_name(c.as_ptr()) }
}

/// A pixel or sample format's number in THIS build, by name. Never hardcoded: `ff.rs` learned that
/// an `FF_API_*` alias shifts the enum between builds.
pub fn pix_fmt(name: &CStr) -> c_int {
    unsafe { av_get_pix_fmt(name.as_ptr()) }
}
fn sample_fmt(name: &CStr) -> c_int {
    unsafe { av_get_sample_fmt(name.as_ptr()) }
}

/// One decoded frame, owned. Dropping it frees it, on whichever thread holds it last.
pub struct Frame(*mut c_void);
// SAFETY: an AVFrame is plain reference-counted data; nothing in it is tied to the thread that
// decoded it, and only one owner touches it at a time.
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { av_frame_free(&mut self.0) };
    }
}

impl Frame {
    unsafe fn i32_at(&self, off: usize) -> i32 {
        *((self.0 as *const u8).add(off) as *const i32)
    }
    pub fn width(&self) -> i32 {
        unsafe { self.i32_at(OFF_FRAME_WIDTH) }
    }
    pub fn height(&self) -> i32 {
        unsafe { self.i32_at(OFF_FRAME_HEIGHT) }
    }
    pub fn format(&self) -> c_int {
        unsafe { self.i32_at(OFF_FRAME_FORMAT) }
    }
    pub fn nb_samples(&self) -> i32 {
        unsafe { self.i32_at(OFF_FRAME_NB_SAMPLES) }
    }
    pub fn sample_rate(&self) -> i32 {
        unsafe { self.i32_at(OFF_FRAME_SAMPLE_RATE) }
    }
    pub fn color_range(&self) -> c_int {
        unsafe { self.i32_at(OFF_FRAME_COLOR_RANGE) }
    }
    pub fn color_trc(&self) -> c_int {
        unsafe { self.i32_at(OFF_FRAME_COLOR_TRC) }
    }
    pub fn colorspace(&self) -> c_int {
        unsafe { self.i32_at(OFF_FRAME_COLORSPACE) }
    }
    /// `sample_aspect_ratio` as (num, den); (0, 1) when unknown.
    pub fn sar(&self) -> (i32, i32) {
        unsafe { (self.i32_at(OFF_FRAME_SAR), self.i32_at(OFF_FRAME_SAR + 4)) }
    }
    /// The presentation time the packet carried in, through the decoder's reordering.
    pub fn pts(&self) -> Option<i64> {
        let read = |off| unsafe { *((self.0 as *const u8).add(off) as *const i64) };
        [read(OFF_FRAME_PTS), read(OFF_FRAME_BEST_EFFORT_TS)].into_iter().find(|&t| t != AV_NOPTS_VALUE)
    }
    pub fn plane(&self, i: usize) -> (*const u8, c_int) {
        unsafe {
            let data = *((self.0 as *const u8).add(OFF_FRAME_DATA) as *const *const u8).add(i);
            let line = *((self.0 as *const u8).add(OFF_FRAME_LINESIZE) as *const c_int).add(i);
            (data, line)
        }
    }
    fn extended_data(&self) -> *const *const u8 {
        unsafe { *((self.0 as *const u8).add(OFF_FRAME_EXTENDED_DATA) as *const *const *const u8) }
    }
    fn ch_layout(&self) -> *const c_void {
        unsafe { (self.0 as *const u8).add(OFF_FRAME_CH_LAYOUT) as *const c_void }
    }
    /// `AVChannelLayout.nb_channels` (the second `int` of the struct).
    pub fn channels(&self) -> i32 {
        unsafe { self.i32_at(OFF_FRAME_CH_LAYOUT + 4) }
    }
}

/// A decoder for one elementary stream of the payload vocabulary (`h264`, `hevc`, `ac3`, `eac3`,
/// `aac`, `dca`), fed one access unit per packet with its presentation time in fed-PTS
/// nanoseconds. FFmpeg carries the timestamp through its reordering untouched, so a frame comes out
/// stamped with the time it is to be shown.
pub struct Decoder {
    ctx: *mut c_void,
    pkt: *mut AVPacket,
    frame: *mut c_void,
    /// The pixel formats a hardware frame arrives in, which must be copied to memory first.
    hw_formats: Vec<c_int>,
    /// What decodes: the OS's hardware decoder by name, or `software`.
    pub engine: String,
}
// SAFETY: the codec context is used by one thread at a time — the lane's own.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.frame);
            av_packet_free(&mut self.pkt);
            avcodec_free_context(&mut self.ctx);
        }
    }
}

impl Decoder {
    /// Open `name`, on the OS's hardware decoder `hw` (`videotoolbox`, `d3d11va`) when one is named
    /// and the device opens, else in software on every core.
    pub fn open(name: &str, hw: Option<&str>) -> Option<Decoder> {
        let codec = find(name);
        if codec.is_null() {
            plx_base::eventlog::log(&format!("desk: no {name} decoder"));
            return None;
        }
        unsafe {
            let mut ctx = avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return None;
            }
            av_opt_set(ctx, c"threads".as_ptr(), c"auto".as_ptr(), 0);
            let mut engine = "software".to_string();
            if let Some(hw) = hw {
                // A device that will not open (no GPU, a remote session, a driver without the
                // profile) leaves the context as it was: software, logged.
                let device = std::ffi::CString::new(hw).ok();
                let ty = device.as_ref().map_or(-1, |d| av_hwdevice_find_type_by_name(d.as_ptr()));
                let mut dev: *mut c_void = std::ptr::null_mut();
                let rc = if ty > 0 { av_hwdevice_ctx_create(&mut dev, ty, std::ptr::null(), std::ptr::null_mut(), 0) } else { -1 };
                if rc >= 0 && !dev.is_null() {
                    // The context owns this reference from here: `avcodec_free_context` unrefs it.
                    // With a device set, libavcodec's default `get_format` picks the matching
                    // hardware format, and falls back to software by itself when the stream's
                    // profile is one the hardware cannot decode.
                    *((ctx as *mut u8).add(OFF_CTX_HW_DEVICE_CTX) as *mut *mut c_void) = dev;
                    engine = hw.to_string();
                } else {
                    plx_base::eventlog::log(&format!("desk: {hw} device unavailable (rc={rc}); {name} decodes in software"));
                }
            }
            if avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0 {
                plx_base::eventlog::log(&format!("desk: {name} decoder refused to open"));
                avcodec_free_context(&mut ctx);
                return None;
            }
            let pkt = av_packet_alloc();
            let frame = av_frame_alloc();
            let hw_formats = [c"videotoolbox_vld", c"d3d11", c"d3d11va_vld", c"dxva2_vld", c"vaapi", c"cuda"]
                .into_iter()
                .map(pix_fmt)
                .filter(|&f| f >= 0)
                .collect();
            Some(Decoder { ctx, pkt, frame, hw_formats, engine })
        }
    }

    /// Hand the decoder one access unit (`None`: the end of the stream, drain). `Err(EAGAIN)`
    /// means its output must be received first.
    pub fn send(&mut self, au: Option<(&[u8], i64)>) -> Result<(), c_int> {
        let rc = unsafe {
            match au {
                Some((data, pts)) => {
                    // A packet with no `buf` is not reference-counted, so libavcodec copies the
                    // bytes before this returns and the slice need not outlive the call.
                    let p = &mut *self.pkt;
                    p.buf = std::ptr::null_mut();
                    p.data = data.as_ptr() as *mut u8;
                    p.size = data.len() as c_int;
                    p.pts = pts;
                    p.dts = AV_NOPTS_VALUE;
                    p.flags = 0;
                    p.side_data = std::ptr::null_mut();
                    p.side_data_elems = 0;
                    avcodec_send_packet(self.ctx, self.pkt)
                }
                None => avcodec_send_packet(self.ctx, std::ptr::null()),
            }
        };
        if rc < 0 {
            Err(rc)
        } else {
            Ok(())
        }
    }

    /// The next decoded frame, in memory (a hardware frame is copied down first). `None` when the
    /// decoder wants more input, has drained, or failed on this frame.
    pub fn receive(&mut self) -> Option<Frame> {
        unsafe {
            if avcodec_receive_frame(self.ctx, self.frame) < 0 {
                return None;
            }
            let out = av_frame_alloc();
            if out.is_null() {
                av_frame_unref(self.frame);
                return None;
            }
            let format = *((self.frame as *const u8).add(OFF_FRAME_FORMAT) as *const c_int);
            if self.hw_formats.contains(&format) {
                let rc = av_hwframe_transfer_data(out, self.frame, 0);
                if rc >= 0 {
                    av_frame_copy_props(out, self.frame);
                }
                av_frame_unref(self.frame);
                if rc < 0 {
                    let mut out = out;
                    av_frame_free(&mut out);
                    return None;
                }
            } else {
                av_frame_move_ref(out, self.frame);
            }
            Some(Frame(out))
        }
    }

    /// Whether a `send` error is only "receive first".
    pub fn is_again(rc: c_int) -> bool {
        rc == eagain()
    }
}

/// Converts decoded audio of any format, rate and layout into interleaved 32-bit float at the
/// output's rate and channel count, rebuilding itself when the input changes shape.
pub struct Resampler {
    swr: *mut c_void,
    input: (c_int, i32, i32),
    out_rate: i32,
    out_channels: i32,
    out_layout: [u64; CH_LAYOUT_BYTES / 8],
    flt: c_int,
}
// SAFETY: owned and used by the audio lane's thread alone.
unsafe impl Send for Resampler {}

impl Drop for Resampler {
    fn drop(&mut self) {
        unsafe {
            swr_free(&mut self.swr);
            av_channel_layout_uninit(self.out_layout.as_mut_ptr() as *mut c_void);
        }
    }
}

impl Resampler {
    pub fn new(out_rate: i32, out_channels: i32) -> Resampler {
        let mut out_layout = [0u64; CH_LAYOUT_BYTES / 8];
        unsafe { av_channel_layout_default(out_layout.as_mut_ptr() as *mut c_void, out_channels) };
        Resampler {
            swr: std::ptr::null_mut(),
            input: (-1, 0, 0),
            out_rate,
            out_channels,
            out_layout,
            flt: sample_fmt(c"flt"),
        }
    }

    /// `frame`'s samples as interleaved f32 (`out_channels` per sample). Empty when it cannot be
    /// converted, which is logged once per shape.
    pub fn convert(&mut self, frame: &Frame) -> Vec<f32> {
        let input = (frame.format(), frame.sample_rate(), frame.channels());
        unsafe {
            if self.swr.is_null() || input != self.input {
                swr_free(&mut self.swr);
                let rc = swr_alloc_set_opts2(
                    &mut self.swr,
                    self.out_layout.as_ptr() as *const c_void,
                    self.flt,
                    self.out_rate,
                    frame.ch_layout(),
                    input.0,
                    input.1,
                    0,
                    std::ptr::null_mut(),
                );
                if rc < 0 || self.swr.is_null() || swr_init(self.swr) < 0 {
                    plx_base::eventlog::log(&format!(
                        "desk: cannot resample fmt={} rate={} channels={} to the output",
                        input.0, input.1, input.2
                    ));
                    swr_free(&mut self.swr);
                    self.input = input;
                    return Vec::new();
                }
                self.input = input;
            }
            if self.swr.is_null() {
                return Vec::new();
            }
            let n = frame.nb_samples();
            let cap = swr_get_out_samples(self.swr, n).max(0);
            let mut out = vec![0f32; cap as usize * self.out_channels as usize];
            let ptr = out.as_mut_ptr() as *mut u8;
            let got = swr_convert(self.swr, &ptr, cap, frame.extended_data(), n);
            out.truncate(got.max(0) as usize * self.out_channels as usize);
            out
        }
    }
}
