//! The desktop's sound output: SDL's queued audio, behind the player's `AudioOut`
//! (`plx_media::player::desk::audio`).
//!
//! Queued rather than a callback because the clock is made of the queue: the decoder's lane appends
//! samples, the device drains them at its own rate, and `SDL_GetQueuedAudioSize` says how far
//! behind the queue's end the card is playing. SDL converts nothing here: the device is opened for
//! 32-bit float, the lane resamples to whatever rate and channel count it grants, and a device that
//! offers a layout the lane cannot name in SDL's channel order (anything but mono, stereo, 5.1 and
//! 7.1) is re-opened as stereo.
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Mutex;

use plx_base::eventlog::log;

#[repr(C)]
struct SdlAudioSpec {
    freq: c_int,
    format: u16,
    channels: u8,
    silence: u8,
    samples: u16,
    padding: u16,
    size: u32,
    callback: Option<extern "C" fn(*mut c_void, *mut u8, c_int)>,
    userdata: *mut c_void,
}

extern "C" {
    fn SDL_InitSubSystem(flags: u32) -> c_int;
    fn SDL_OpenAudioDevice(
        device: *const c_char,
        iscapture: c_int,
        desired: *const SdlAudioSpec,
        obtained: *mut SdlAudioSpec,
        allowed_changes: c_int,
    ) -> u32;
    fn SDL_CloseAudioDevice(dev: u32);
    fn SDL_PauseAudioDevice(dev: u32, pause_on: c_int);
    fn SDL_QueueAudio(dev: u32, data: *const c_void, len: u32) -> c_int;
    fn SDL_GetQueuedAudioSize(dev: u32) -> u32;
    fn SDL_ClearQueuedAudio(dev: u32);
    fn SDL_GetError() -> *const c_char;
}

const SDL_INIT_AUDIO: u32 = 0x10;
/// `AUDIO_F32SYS` on every little-endian desktop (`AUDIO_F32LSB`).
const AUDIO_F32: u16 = 0x8120;
const ALLOW_FREQUENCY_CHANGE: c_int = 0x1;
const ALLOW_CHANNELS_CHANGE: c_int = 0x4;
/// The device buffer, in sample frames: about 21 ms at 48 kHz.
const DEVICE_FRAMES: u16 = 1024;

/// The open device: (id, rate, channels, the shape it was asked for). Id 0 = none.
static DEVICE: Mutex<(u32, i32, i32, (i32, i32))> = Mutex::new((0, 0, 0, (0, 0)));

pub struct SdlAudio;
pub static OUTPUT: SdlAudio = SdlAudio;

fn error() -> String {
    unsafe { std::ffi::CStr::from_ptr(SDL_GetError()).to_string_lossy().into_owned() }
}

unsafe fn open_device(rate: i32, channels: i32, allowed: c_int) -> Option<(u32, i32, i32)> {
    let want = SdlAudioSpec {
        freq: rate,
        format: AUDIO_F32,
        channels: channels as u8,
        silence: 0,
        samples: DEVICE_FRAMES,
        padding: 0,
        size: 0,
        callback: None,
        userdata: std::ptr::null_mut(),
    };
    let mut got = SdlAudioSpec { ..want };
    let dev = SDL_OpenAudioDevice(std::ptr::null(), 0, &want, &mut got, allowed);
    (dev != 0).then_some((dev, got.freq, c_int::from(got.channels)))
}

impl plx_media::player::desk::audio::AudioOut for SdlAudio {
    fn open(&self, rate: i32, channels: i32) -> Option<(i32, i32)> {
        let mut d = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            if d.0 != 0 && d.3 == (rate, channels) {
                SDL_PauseAudioDevice(d.0, 1);
                SDL_ClearQueuedAudio(d.0);
                return Some((d.1, d.2));
            }
            if d.0 != 0 {
                SDL_CloseAudioDevice(d.0);
                *d = (0, 0, 0, (0, 0));
            }
            if SDL_InitSubSystem(SDL_INIT_AUDIO) != 0 {
                log(&format!("audio: SDL audio unavailable ({})", error()));
                return None;
            }
            let mut opened = open_device(rate, channels, ALLOW_FREQUENCY_CHANGE | ALLOW_CHANNELS_CHANGE);
            if let Some((dev, _, ch)) = opened {
                if ![1, 2, 6, 8].contains(&ch) {
                    SDL_CloseAudioDevice(dev);
                    opened = open_device(rate, 2, ALLOW_FREQUENCY_CHANGE);
                }
            }
            let Some((dev, got_rate, got_ch)) = opened else {
                log(&format!("audio: no output device ({})", error()));
                return None;
            };
            log(&format!("audio: device open {got_ch}ch {got_rate}Hz (asked {channels}ch {rate}Hz)"));
            *d = (dev, got_rate, got_ch, (rate, channels));
            Some((got_rate, got_ch))
        }
    }

    fn queue(&self, samples: &[f32]) {
        let d = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        if d.0 != 0 {
            unsafe { SDL_QueueAudio(d.0, samples.as_ptr() as *const c_void, (samples.len() * 4) as u32) };
        }
    }

    fn queued_bytes(&self) -> u32 {
        let d = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        if d.0 == 0 {
            0
        } else {
            unsafe { SDL_GetQueuedAudioSize(d.0) }
        }
    }

    fn device_frames(&self) -> u32 {
        u32::from(DEVICE_FRAMES)
    }

    fn clear(&self) {
        let d = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        if d.0 != 0 {
            unsafe { SDL_ClearQueuedAudio(d.0) };
        }
    }

    fn set_paused(&self, paused: bool) {
        let d = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        if d.0 != 0 {
            unsafe { SDL_PauseAudioDevice(d.0, c_int::from(paused)) };
        }
    }
}
