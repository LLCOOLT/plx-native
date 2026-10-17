//! **The desktop player's volume and mute.**
//!
//! A television's volume is the set's own — its remote's rocker talks to the panel, not to this
//! app — so nothing on webOS reads or writes this. On a desktop the app IS the volume control a
//! viewer reaches for: the player's desktop transport (`appkit::player_hud`'s pointer row, through
//! `PlayerReq::Volume`) sets it, and the desktop decoder's audio lane (`desk::audio`) applies
//! [`gain`] to every sample it queues. Process-wide like the sound card it describes, and not
//! logical state: no screen's behaviour depends on it, so it is not in any recording's canon.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

static LEVEL_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static MUTED: AtomicBool = AtomicBool::new(false);

/// The slider's position, 0 to 1.
pub fn level() -> f32 {
    f32::from_bits(LEVEL_BITS.load(Relaxed))
}

pub fn muted() -> bool {
    MUTED.load(Relaxed)
}

/// Move the slider. Moving it off zero unmutes — dragging a muted slider up is asking to hear it.
pub fn set_level(level: f32) {
    let level = if level.is_finite() { level.clamp(0.0, 1.0) } else { 1.0 };
    LEVEL_BITS.store(level.to_bits(), Relaxed);
    if level > 0.0 {
        MUTED.store(false, Relaxed);
    }
}

pub fn toggle_mute() {
    MUTED.fetch_xor(true, Relaxed);
}

/// What each sample is multiplied by. Squared, so the slider's travel is spread over what a
/// listener hears as loudness rather than crowded into its top quarter; exactly 1 at full.
pub fn gain() -> f32 {
    if muted() {
        0.0
    } else {
        let l = level();
        l * l
    }
}

/// Is the speaker silent — muted, or the slider at zero? The control's glyph reads this.
pub fn silent() -> bool {
    gain() == 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the state is process-wide: a second test racing it would read its writes.
    #[test]
    fn the_slider_mute_and_gain_agree() {
        set_level(1.0);
        if muted() {
            toggle_mute();
        }
        assert_eq!(gain(), 1.0, "full and unmuted plays the samples unchanged");
        set_level(0.5);
        assert_eq!(gain(), 0.25);
        toggle_mute();
        assert!(muted() && silent() && gain() == 0.0);
        assert_eq!(level(), 0.5, "muting keeps the slider where it was");
        set_level(0.8);
        assert!(!muted(), "moving the slider up unmutes");
        set_level(0.0);
        assert!(silent() && !muted(), "zero is silent without being muted");
        set_level(f32::NAN);
        assert_eq!(level(), 1.0);
        set_level(2.0);
        assert_eq!(level(), 1.0);
        set_level(1.0);
    }
}
