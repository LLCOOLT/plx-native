//! The progressive demuxer fed an MPEG-TS source — a Live TV channel from Tunarr, whose H.264 or
//! HEVC is Annex-B with its parameter sets in band and whose AAC is already ADTS-framed. The H.264
//! half of start-code framing is graded in `ff_packet_framing_tests.rs`; this file grades what a
//! channel adds: HEVC in band, the unstamped AAC frames of a PES, and the analysis window.

use super::*;

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// HEVC in MPEG-TS: VPS/SPS/PPS (types 32/33/34) are kept from the stream and put in front of an
/// IRAP (16..=23) that lacks them.
#[test]
fn a_tunarr_hevc_keyframe_gets_its_vps_sps_pps() {
    let vps = [0, 0, 0, 1, 0x40, 0x01, 0xaa];
    let sps = [0, 0, 0, 1, 0x42, 0x01, 0xbb];
    let pps = [0, 0, 0, 1, 0x44, 0x01, 0xcc];
    let first = cat(&[&vps, &sps, &pps, &[0, 0, 0, 1, 0x26, 0x01, 1]]); // IDR_W_RADL (19)
    let mut params = H264ParamSets::default();
    let mut out = Vec::new();
    assert_eq!(annexb_hevc_access_unit(&first, &mut params, &mut out), Ok(true));
    assert_eq!(out, first);
    let later = [0, 0, 0, 1, 0x28, 0x01, 2]; // CRA (20)
    assert_eq!(annexb_hevc_access_unit(&later, &mut params, &mut out), Ok(true));
    assert_eq!(out, cat(&[&vps, &sps, &pps, &later]));
    let trail = [0, 0, 0, 1, 0x02, 0x01, 3]; // TRAIL_R (1)
    assert_eq!(annexb_hevc_access_unit(&trail, &mut params, &mut out), Ok(false));
    assert_eq!(out, trail, "a non-key picture passes through untouched");
}

/// Joining an HEVC channel mid-GOP with no parameter set seen yet: that IRAP cannot be decoded
/// and is refused (the demuxer drops it); the next one carrying its sets plays.
#[test]
fn an_hevc_irap_with_no_parameter_sets_anywhere_is_refused_until_one_arrives() {
    let mut params = H264ParamSets::default();
    let mut out = Vec::new();
    assert!(annexb_hevc_access_unit(&[0, 0, 0, 1, 0x26, 0x01, 1], &mut params, &mut out).is_err());
    assert!(annexb_hevc_access_unit(&[0x26, 0x01], &mut params, &mut out).is_err(), "no start code");
    let vps = [0, 0, 0, 1, 0x40, 0x01, 0xaa];
    let sps = [0, 0, 0, 1, 0x42, 0x01, 0xbb];
    let pps = [0, 0, 0, 1, 0x44, 0x01, 0xcc];
    let idr = cat(&[&vps, &sps, &pps, &[0, 0, 0, 1, 0x26, 0x01, 2]]);
    assert_eq!(annexb_hevc_access_unit(&idr, &mut params, &mut out), Ok(true));
    assert_eq!(out, idr);
}

/// MPEG-TS AAC is already ADTS; a second header in front of it is a frame LG's decoder cannot
/// parse, and with audioSync a dead audio clock holds the video too.
#[test]
fn an_adts_frame_from_mpeg_ts_is_not_wrapped_twice() {
    let mut adts = adts_header(4, 2, 5).to_vec();
    adts.extend_from_slice(&[0x21, 0x10, 0x05, 0x00, 0xa0]);
    assert_eq!(adts_framed(&adts, Some((4, 2))).unwrap().as_ref(), &adts[..]);
    let raw = [0x21, 0x10, 0x05, 0x00, 0xa0];
    let framed = adts_framed(&raw, Some((4, 2))).unwrap();
    assert_eq!(&framed[..7], &adts_header(4, 2, 5));
    assert_eq!(&framed[7..], &raw);
}

/// Tunarr's MPEG-TS packs several AAC frames into one PES, and the PES timestamp belongs to the
/// first frame that starts in it. libavformat stamps the others from the frame duration once its
/// own running clock is set, but not before it; the progressive demuxer used to feed such a frame
/// at time 0. A frame without a timestamp starts where the one before it ended.
#[test]
fn an_aac_frame_without_a_timestamp_starts_where_the_last_one_ended() {
    const FRAME: i64 = 21_333_333; // 1024 samples at 48 kHz
    let mut clock = ProgressiveAudioClock::default();
    assert_eq!(clock.stamp(Some(900_000_000), Some(FRAME)), Some(900_000_000));
    assert_eq!(clock.stamp(None, Some(FRAME)), Some(900_000_000 + FRAME));
    assert_eq!(clock.stamp(None, Some(FRAME)), Some(900_000_000 + 2 * FRAME));
    // The next PES carries its own timestamp again, and it wins.
    assert_eq!(clock.stamp(Some(1_000_000_000), Some(FRAME)), Some(1_000_000_000));
    assert_eq!(clock.stamp(None, Some(FRAME)), Some(1_000_000_000 + FRAME));
}

/// Joined mid-PES, the first frames have nothing to count from: they are dropped rather than fed
/// at 0. After a seek the count starts again from the next timestamp.
#[test]
fn an_unanchored_aac_frame_is_dropped_and_a_seek_forgets_the_anchor() {
    let mut clock = ProgressiveAudioClock::default();
    assert_eq!(clock.stamp(None, Some(20_000_000)), None);
    assert_eq!(clock.stamp(Some(5_000_000_000), Some(20_000_000)), Some(5_000_000_000));
    clock.reset();
    assert_eq!(clock.stamp(None, Some(20_000_000)), None);
    // A frame of unknown length leaves the next one with nothing to count from.
    assert_eq!(clock.stamp(Some(7_000_000_000), None), Some(7_000_000_000));
    assert_eq!(clock.stamp(None, Some(20_000_000)), None);
}

/// The frame length comes from the packet when FFmpeg knows it, and from the ADTS header when it
/// does not (1024 samples per raw data block, at the header's own rate).
#[test]
fn an_aac_frame_length_is_read_from_the_packet_or_its_adts_header() {
    let mut adts = adts_header(3, 2, 5).to_vec(); // 48 kHz
    adts.extend_from_slice(&[0x21, 0x10, 0x05, 0x00, 0xa0]);
    assert_eq!(audio_frame_ns(Some(23_000_000), &adts), Some(23_000_000));
    assert_eq!(audio_frame_ns(None, &adts), Some(21_333_333));
    assert_eq!(audio_frame_ns(None, &[0x0b, 0x77, 0, 0, 0, 0, 0]), None);
}

/// A channel's analysis window is one second: what a real-time stream can spare before its first
/// packet, and enough for in-band parameter sets and ADTS headers.
#[test]
fn a_live_stream_is_analysed_for_one_second() {
    assert_eq!(live_analyzeduration_us(), 1_000_000);
}
