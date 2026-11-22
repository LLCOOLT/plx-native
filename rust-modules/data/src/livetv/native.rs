//! **Tunarr native mode**: play what a Tunarr channel airs from the Plex file itself, at the point
//! the channel is at, instead of Tunarr's transcoded MPEG-TS stream.
//!
//! Tunarr already knows what is on: `GET /api/channels/{uuid}/native-playback`
//! (`server/src/api/nativePlaybackApi.ts`) answers with the programme airing now, how far into its
//! file the channel is (`seekOffsetMs`) and how long it has left (`remainingMs`), all at Tunarr's
//! own `serverTimeMs`. When that programme is an item on a Plex server the app is signed into, the
//! app plays the original file through the same quiet path a virtual channel uses — direct play
//! where the television can, no second encode, no PlayQueue, no scrobble — and Tunarr keeps the
//! clock, so the guide, other apps and this one agree on what is on.
//!
//! Which Plex item it is comes from the reply's optional `source` (`serverIdentifier`, the Plex
//! `machineIdentifier`, and `externalKey`, the ratingKey), which newer Tunarr builds send. An older
//! build sends neither: the programme is then read from `GET /api/programs/{id}` (`sourceType`
//! `plex`, `externalId` = ratingKey, `mediaSourceId`), and the server is found by asking the
//! signed-in servers for that ratingKey and keeping the one whose item carries the programme's
//! title. Tunarr's media-source list is deliberately NOT read: it carries the Plex access tokens
//! Tunarr holds, which this app has no business receiving.
//!
//! Everything that is not a Plex item the app can reach — flex (filler, offline), an error item,
//! a Jellyfin/Emby/local programme, a server not signed in, an older Tunarr that 404s the route —
//! is a [`Plan::Stream`]: the channel's ordinary `.ts` stream plays exactly as before.
//!
//! Pure apart from [`resolve`], which BLOCKS (two or three HTTP round trips) and runs on a worker.

use plx_plex::http::{self, Method};
use plx_plex::plex::{Origin, ServerId};
use serde_json::Value;
use std::collections::HashMap;

/// The channel a lineup URL names: Tunarr's HDHomeRun lineup points each channel at
/// `{origin}/stream/channels/{uuid}.ts` (optionally with a query). `None` for any other shape —
/// a non-Tunarr HDHomeRun device, or a virtual channel.
pub fn channel_of(stream_url: &str) -> Option<(String, String)> {
    let origin = super::hdhr::origin_of(stream_url)?;
    let rest = stream_url.strip_prefix(&origin)?;
    let path = rest.split(['?', '#']).next()?;
    let id = path.strip_prefix("/stream/channels/")?.strip_suffix(".ts")?;
    let plausible = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plausible.then(|| (origin, id.to_owned()))
}

/// Where a programme's original file lives, as `native-playback` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemSource {
    /// `plex`, `jellyfin` or `emby`.
    pub kind: String,
    pub media_source_id: String,
    /// The Plex server's `machineIdentifier`, when Tunarr has stored it.
    pub server_identifier: Option<String>,
    /// The item's key on that server: the Plex ratingKey.
    pub external_key: String,
}

/// One item of a `native-playback` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Content {
        program_id: String,
        /// The show for an episode, else the programme's own title.
        title: String,
        /// The episode's own title, for an episode.
        episode_title: Option<String>,
        season: Option<i64>,
        episode: Option<i64>,
        /// Where in the FILE the channel is at the reply's `serverTimeMs`.
        seek_offset_ms: i64,
        /// How long the channel stays on this programme from `serverTimeMs` (the schedule may cut
        /// it short of the file's end).
        remaining_ms: i64,
        source: Option<ItemSource>,
    },
    /// Filler, an offline screen or a redirect Tunarr draws itself.
    Flex { remaining_ms: i64 },
    /// Tunarr could not work out what is on.
    Error { retry_after_ms: i64 },
}

/// A `native-playback` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub server_time_ms: i64,
    pub current: Item,
}

fn int(v: &Value, key: &str) -> Option<i64> {
    let n = v.get(key)?;
    n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
}

fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn item_of(v: &Value) -> Option<Item> {
    match v.get("type")?.as_str()? {
        "content" => Some(Item::Content {
            program_id: string(v, "programId")?,
            title: string(v, "title").unwrap_or_default(),
            episode_title: string(v, "episodeTitle"),
            season: int(v, "seasonNumber"),
            episode: int(v, "episodeNumber"),
            seek_offset_ms: int(v, "seekOffsetMs").unwrap_or(0).max(0),
            remaining_ms: int(v, "remainingMs")?.max(0),
            source: v.get("source").and_then(|s| {
                Some(ItemSource {
                    kind: string(s, "type")?,
                    media_source_id: string(s, "mediaSourceId").unwrap_or_default(),
                    server_identifier: string(s, "serverIdentifier"),
                    external_key: string(s, "externalKey")?,
                })
            }),
        }),
        "flex" => Some(Item::Flex { remaining_ms: int(v, "remainingMs").unwrap_or(0).max(0) }),
        "error" => Some(Item::Error { retry_after_ms: int(v, "retryAfterMs").unwrap_or(5_000).max(0) }),
        // A kind a later Tunarr adds is not something this app knows how to play itself.
        _ => None,
    }
}

/// Parse a `native-playback` body. Unknown fields are ignored; `None` when the body is not a
/// reply at all (an HTML error page, an unknown `current` kind).
pub fn parse_reply(body: &[u8]) -> Option<Reply> {
    let v: Value = serde_json::from_slice(body).ok()?;
    Some(Reply { server_time_ms: int(&v, "serverTimeMs")?, current: item_of(v.get("current")?)? })
}

/// What `GET /api/programs/{id}` says about a Plex programme: its ratingKey and which of Tunarr's
/// media sources it came from. `None` for a programme from anything but Plex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramRef {
    pub rating_key: String,
    pub media_source_id: String,
}

pub fn parse_program(body: &[u8]) -> Option<ProgramRef> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v.get("sourceType").and_then(Value::as_str) != Some("plex") {
        return None;
    }
    Some(ProgramRef { rating_key: string(&v, "externalId")?, media_source_id: string(&v, "mediaSourceId").unwrap_or_default() })
}

/// Tunarr's clock against ours, measured on one request: the reply's `serverTimeMs` was taken
/// somewhere between our `sent_ms` and `received_ms` (local wall clock), and the midpoint is the
/// best estimate of when. Every Tunarr instant is mapped onto the local clock through it, so a
/// television whose clock is off (they routinely are, by seconds) still joins at the right point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// The local instant the reply's `serverTimeMs` corresponds to.
    pub local_ms: i64,
    /// `local - server`, in ms.
    pub skew_ms: i64,
}

impl Clock {
    pub fn measure(sent_ms: i64, received_ms: i64, server_time_ms: i64) -> Self {
        let local_ms = sent_ms + (received_ms - sent_ms).max(0) / 2;
        Self { local_ms, skew_ms: local_ms - server_time_ms }
    }
}

/// Where in the file the channel is at local instant `now_ms`, for an item that was at
/// `seek_offset_ms` when the reply was taken.
pub fn file_offset_at(clock: &Clock, seek_offset_ms: i64, now_ms: i64) -> i64 {
    (seek_offset_ms + (now_ms - clock.local_ms).max(0)).max(0)
}

/// Episode titles match the Plex item: a show's episode is `(show, Some(episode))` in Tunarr and
/// `title` = episode, `grandparentTitle` = show in Plex; a film is its own title in both. Compared
/// case-insensitively and ignoring surrounding space. Either half of an episode is enough: a
/// renamed episode on one side should not throw away a ratingKey that is plainly the item.
pub fn titles_match(title: &str, episode_title: Option<&str>, plex_title: &str, plex_show_title: &str) -> bool {
    let eq = |a: &str, b: &str| !a.trim().is_empty() && a.trim().to_lowercase() == b.trim().to_lowercase();
    match episode_title {
        Some(ep) => eq(ep, plex_title) || eq(title, plex_show_title),
        None => eq(title, plex_title),
    }
}

/// A programme the app plays itself, from the Plex file.
#[derive(Clone, Debug, PartialEq)]
pub struct Direct {
    pub sid: ServerId,
    pub rk: String,
    pub part: String,
    pub vcodec: String,
    pub acodec: String,
    /// The HUD's title and context line.
    pub title: String,
    pub ctx: String,
    pub program_id: String,
    pub clock: Clock,
    /// Where in the file the channel was at the reply.
    pub seek_offset_ms: i64,
    /// The FILE position at which the channel leaves this programme (`seek + remaining`): the
    /// programme ends there whether the viewer is live or has paused behind it.
    pub end_offset_ms: i64,
}

impl Direct {
    /// The local wall instant the file's position 0 would have aired — the virtual channels'
    /// `airing_start_ms`, so BEHIND and jump-to-live read the same way.
    pub fn airing_start_ms(&self) -> i64 {
        self.clock.local_ms - self.seek_offset_ms
    }
}

/// Why the ordinary stream plays instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// The Tunarr build has no `native-playback` route (404/405): nothing on this origin will.
    Unsupported,
    /// Tunarr's own filler or offline screen is on, for `remaining_ms`: worth asking again then.
    Flex { remaining_ms: i64 },
    /// Not a Plex item this app can reach (another media source, a server not signed in, a
    /// ratingKey that no server holds), or Tunarr could not say.
    NotPlex,
    /// The request or its answer failed.
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Direct(Direct),
    Stream(Why),
}

/// The local instant after which a stream that plays because of `why` should be tuned natively
/// again, if ever: only flex ends on its own, and never sooner than [`RECHECK_MIN_MS`] after
/// `now_ms`, so a run of very short filler cannot turn into a tune loop.
pub fn recheck_at(why: &Why, now_ms: i64) -> Option<i64> {
    match why {
        Why::Flex { remaining_ms } => Some(now_ms + (*remaining_ms).max(RECHECK_MIN_MS)),
        _ => None,
    }
}

/// The shortest wait before a flex stream is asked about again.
pub const RECHECK_MIN_MS: i64 = 5_000;

/// The HUD lines for a Tunarr programme, the way virtual channels write them: the show with
/// "S2 · E5 · Title" under it, or the film with its year.
pub fn titles_for(title: &str, episode_title: Option<&str>, season: Option<i64>, episode: Option<i64>, year: i64) -> (String, String) {
    match episode_title {
        Some(ep) => {
            let code = match (season, episode) {
                (Some(s), Some(e)) => format!("S{s} \u{b7} E{e}"),
                (None, Some(e)) => format!("E{e}"),
                _ => String::new(),
            };
            let ctx = [code.as_str(), ep].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" \u{b7} ");
            (title.to_owned(), ctx)
        }
        None => (title.to_owned(), if year > 0 { year.to_string() } else { String::new() }),
    }
}

/// The servers to ask for a ratingKey when the reply did not say which: one already learned for
/// this Tunarr media source first, then the current server, then the rest, each once.
pub fn candidate_servers(learned: Option<ServerId>, current: ServerId, all: &[ServerId]) -> Vec<ServerId> {
    let mut out: Vec<ServerId> = Vec::new();
    for id in learned.into_iter().chain(std::iter::once(current)).chain(all.iter().copied()) {
        if id != ServerId::UNSET && all.contains(&id) && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The play fields of a fetched Plex item: its first version's first part and codecs, its year,
/// and its two titles. `None` for an item with no playable part.
fn play_fields(m: &plx_plex::plex::Metadata) -> Option<(String, String, String)> {
    let media = m.media.first()?;
    let part = media.part.first()?.key.clone();
    (!part.is_empty()).then(|| (part, media.video_codec.clone(), media.audio_codec.clone()))
}

fn get_json(origin: &Origin, path: &str) -> Result<Vec<u8>, i32> {
    let reply = http::request(origin, path, Method::Get, &["Accept: application/json"], None).ok_or(0)?;
    if (200..300).contains(&reply.status) { Ok(reply.body) } else { Err(reply.status) }
}

/// **What to play for the Tunarr channel streamed at `stream_url`, now.** BLOCKS. `learned` maps
/// Tunarr media-source ids to the server they turned out to be (the caller keeps it between tunes);
/// a newly learned pair is returned beside the plan.
pub fn resolve(stream_url: &str, learned: &HashMap<String, ServerId>) -> (Plan, Option<(String, ServerId)>) {
    let Some((origin_s, channel)) = channel_of(stream_url) else { return (Plan::Stream(Why::NotPlex), None) };
    let Some(origin) = Origin::parse(&origin_s) else { return (Plan::Stream(Why::Failed), None) };
    let sent = plx_base::wallclock::now_ms();
    let body = match get_json(&origin, &format!("/api/channels/{channel}/native-playback")) {
        Ok(b) => b,
        Err(404 | 405) => return (Plan::Stream(Why::Unsupported), None),
        Err(_) => return (Plan::Stream(Why::Failed), None),
    };
    let received = plx_base::wallclock::now_ms();
    let Some(reply) = parse_reply(&body) else { return (Plan::Stream(Why::Failed), None) };
    let clock = Clock::measure(sent, received, reply.server_time_ms);
    let (program_id, title, episode_title, season, episode, seek, remaining, source) = match reply.current {
        Item::Content { program_id, title, episode_title, season, episode, seek_offset_ms, remaining_ms, source } => {
            (program_id, title, episode_title, season, episode, seek_offset_ms, remaining_ms, source)
        }
        Item::Flex { remaining_ms } => return (Plan::Stream(Why::Flex { remaining_ms }), None),
        Item::Error { .. } => return (Plan::Stream(Why::NotPlex), None),
    };
    // Which Plex item, and (when Tunarr says) which server.
    let (rk, media_source, named_server) = match source {
        Some(s) if s.kind == "plex" => (s.external_key, s.media_source_id, s.server_identifier),
        Some(_) => return (Plan::Stream(Why::NotPlex), None),
        None => match get_json(&origin, &format!("/api/programs/{program_id}")).ok().and_then(|b| parse_program(&b)) {
            Some(p) => (p.rating_key, p.media_source_id, None),
            None => return (Plan::Stream(Why::NotPlex), None),
        },
    };
    let all: Vec<ServerId> = plx_plex::plex::server_ids().collect();
    let candidates = match named_server.as_deref().and_then(plx_plex::plex::id_of_machine) {
        Some(sid) => vec![sid],
        // Tunarr named a server the app is not signed into: no other server's item is this one.
        None if named_server.is_some() => return (Plan::Stream(Why::NotPlex), None),
        None => candidate_servers(learned.get(&media_source).copied(), plx_plex::plex::current_server(), &all),
    };
    let named = named_server.is_some();
    for sid in candidates {
        let Some(meta) = plx_plex::plex::client_for(sid).and_then(|c| c.metadata(&rk)) else { continue };
        // A server Tunarr named is trusted; one we guessed must carry the programme's title.
        if !named && !titles_match(&title, episode_title.as_deref(), &meta.title, &meta.grandparent_title) {
            continue;
        }
        let Some((part, vcodec, acodec)) = play_fields(&meta) else { continue };
        let (hud_title, ctx) = titles_for(&title, episode_title.as_deref(), season, episode, meta.year);
        let learnt = (!named && !media_source.is_empty() && learned.get(&media_source) != Some(&sid)).then(|| (media_source.clone(), sid));
        let direct = Direct {
            sid,
            rk: rk.clone(),
            part,
            vcodec,
            acodec,
            title: hud_title,
            ctx,
            program_id,
            clock,
            seek_offset_ms: seek,
            end_offset_ms: seek + remaining,
        };
        return (Plan::Direct(direct), learnt);
    }
    (Plan::Stream(Why::NotPlex), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTENT_NEW: &str = r#"{
        "channelId": "5d6c2a9e-1f1a-4c3e-9a53-0b6f9e1d2c3b", "channelNumber": 4, "channelName": "Comedy",
        "serverTimeMs": 1700000000000,
        "current": {"type": "content", "itemStartedAtMs": 1699999400000, "seekOffsetMs": 600000,
            "remainingMs": 720000, "programId": "0b8f4b9c-2b7e-4d0c-9a4a-8c3f1e2d3c4b",
            "title": "The Show", "episodeTitle": "Pilot", "seasonNumber": 1, "episodeNumber": 1,
            "streamUrl": "http://192.0.2.20:8000/stream/channels/x/item-stream.ts?t=1",
            "durationMs": 1320000, "futureField": {"a": 1},
            "source": {"type": "plex", "mediaSourceId": "ms-1", "mediaSourceName": "Home",
                "serverIdentifier": "abc123", "externalKey": "4242"}},
        "next": {"type": "flex", "itemStartedAtMs": 1700000720000, "remainingMs": 30000}
    }"#;

    #[test]
    fn a_lineup_url_names_its_channel() {
        assert_eq!(
            channel_of("http://192.0.2.20:8000/stream/channels/5d6c2a9e-1f1a-4c3e-9a53-0b6f9e1d2c3b.ts"),
            Some(("http://192.0.2.20:8000".to_owned(), "5d6c2a9e-1f1a-4c3e-9a53-0b6f9e1d2c3b".to_owned()))
        );
        assert_eq!(channel_of("http://tunarr.lan/stream/channels/abc.ts?audioOnly=false").map(|c| c.1), Some("abc".to_owned()));
        assert_eq!(channel_of("http://192.0.2.20:8000/stream/channels/../x.ts"), None);
        assert_eq!(channel_of("http://hdhr.lan:5004/auto/v4.1"), None);
        assert_eq!(channel_of("plxvc:123"), None);
    }

    #[test]
    fn a_reply_with_its_source_parses_and_ignores_what_it_does_not_know() {
        let r = parse_reply(CONTENT_NEW.as_bytes()).expect("reply");
        assert_eq!(r.server_time_ms, 1_700_000_000_000);
        let Item::Content { program_id, title, episode_title, season, episode, seek_offset_ms, remaining_ms, source } = r.current else {
            panic!("content")
        };
        assert_eq!(program_id, "0b8f4b9c-2b7e-4d0c-9a4a-8c3f1e2d3c4b");
        assert_eq!((title.as_str(), episode_title.as_deref()), ("The Show", Some("Pilot")));
        assert_eq!((season, episode, seek_offset_ms, remaining_ms), (Some(1), Some(1), 600_000, 720_000));
        let s = source.expect("source");
        assert_eq!((s.kind.as_str(), s.server_identifier.as_deref(), s.external_key.as_str()), ("plex", Some("abc123"), "4242"));
    }

    #[test]
    fn an_older_tunarr_reply_has_no_source_and_flex_and_error_items_parse() {
        let old = r#"{"serverTimeMs": 5, "current": {"type": "content", "itemStartedAtMs": 0,
            "seekOffsetMs": 5, "remainingMs": 10, "programId": "p", "title": "Film", "streamUrl": "u"}}"#;
        let Some(Reply { current: Item::Content { source, episode_title, .. }, .. }) = parse_reply(old.as_bytes()) else { panic!() };
        assert_eq!((source, episode_title), (None, None));
        let flex = r#"{"serverTimeMs": 5, "current": {"type": "flex", "itemStartedAtMs": 0, "remainingMs": 9000}}"#;
        assert_eq!(parse_reply(flex.as_bytes()).map(|r| r.current), Some(Item::Flex { remaining_ms: 9000 }));
        let err = r#"{"serverTimeMs": 5, "current": {"type": "error", "message": "x", "retryAfterMs": 5000}}"#;
        assert_eq!(parse_reply(err.as_bytes()).map(|r| r.current), Some(Item::Error { retry_after_ms: 5000 }));
        assert_eq!(parse_reply(br#"{"serverTimeMs": 5, "current": {"type": "hologram"}}"#), None);
        assert_eq!(parse_reply(b"<html>Not Found</html>"), None);
    }

    #[test]
    fn a_plex_program_names_its_rating_key_and_others_do_not() {
        let plex = br#"{"uuid": "p", "sourceType": "plex", "externalId": "4242", "mediaSourceId": "ms-1", "title": "Film"}"#;
        assert_eq!(parse_program(plex), Some(ProgramRef { rating_key: "4242".into(), media_source_id: "ms-1".into() }));
        assert_eq!(parse_program(br#"{"sourceType": "jellyfin", "externalId": "j1"}"#), None);
        assert_eq!(parse_program(br#"{"sourceType": "local", "externalId": "/films/a.mkv"}"#), None);
    }

    /// The reply's instant is the request's midpoint, so a set whose clock is 7 s fast still joins
    /// where Tunarr is.
    #[test]
    fn the_clock_maps_tunarr_time_onto_the_local_clock() {
        // Tunarr says 1_000_000; we sent at 1_007_000 (7 s fast) and heard back 200 ms later.
        let c = Clock::measure(1_007_000, 1_007_200, 1_000_000);
        assert_eq!(c, Clock { local_ms: 1_007_100, skew_ms: 7_100 });
        assert_eq!(file_offset_at(&c, 600_000, 1_007_100), 600_000);
        // Two seconds later (the resolve took that long) the file is two seconds further in.
        assert_eq!(file_offset_at(&c, 600_000, 1_009_100), 602_000);
        // A reply that arrived "before" it was sent (a clock step) never goes backwards.
        let stepped = Clock::measure(10, 5, 0);
        assert_eq!(stepped.local_ms, 10);
        assert_eq!(file_offset_at(&stepped, 100, 0), 100);
    }

    #[test]
    fn behind_reads_from_the_file_origin_on_the_local_clock() {
        let c = Clock::measure(1_000_000, 1_000_000, 900_000);
        let d = Direct {
            sid: ServerId::UNSET, rk: "1".into(), part: "/p".into(), vcodec: "h264".into(), acodec: "aac".into(),
            title: String::new(), ctx: String::new(), program_id: "p".into(), clock: c,
            seek_offset_ms: 60_000, end_offset_ms: 90_000,
        };
        assert_eq!(d.airing_start_ms(), 940_000);
    }

    #[test]
    fn a_guessed_server_must_carry_the_programmes_title() {
        assert!(titles_match("The Show", Some("Pilot"), "Pilot", "The Show"));
        assert!(titles_match("the show ", Some("Renamed"), "Pilot", "The Show"), "the show alone is enough");
        assert!(titles_match("Film", None, " film", ""));
        assert!(!titles_match("Film", None, "Other Film", ""));
        assert!(!titles_match("", None, "", ""), "nothing to compare is not a match");
        assert!(!titles_match("The Show", Some("Pilot"), "Something", "Other Show"));
    }

    #[test]
    fn the_learned_server_is_asked_first_then_the_current_one_each_once() {
        let (a, b, c) = (ServerId::from_raw(1), ServerId::from_raw(2), ServerId::from_raw(3));
        assert_eq!(candidate_servers(None, b, &[a, b, c]), vec![b, a, c]);
        assert_eq!(candidate_servers(Some(c), b, &[a, b, c]), vec![c, b, a]);
        // A learned or current id no longer signed in is skipped.
        assert_eq!(candidate_servers(Some(ServerId::from_raw(9)), ServerId::UNSET, &[a]), vec![a]);
        assert!(candidate_servers(None, ServerId::UNSET, &[]).is_empty());
    }

    #[test]
    fn only_flex_is_asked_about_again_and_never_in_a_tight_loop() {
        assert_eq!(recheck_at(&Why::Flex { remaining_ms: 30_000 }, 100), Some(30_100));
        assert_eq!(recheck_at(&Why::Flex { remaining_ms: 0 }, 100), Some(100 + RECHECK_MIN_MS));
        assert_eq!(recheck_at(&Why::Unsupported, 100), None);
        assert_eq!(recheck_at(&Why::NotPlex, 100), None);
        assert_eq!(recheck_at(&Why::Failed, 100), None);
    }

    #[test]
    fn the_hud_lines_read_like_a_virtual_channels() {
        assert_eq!(titles_for("The Show", Some("Pilot"), Some(1), Some(2), 0), ("The Show".into(), "S1 \u{b7} E2 \u{b7} Pilot".into()));
        assert_eq!(titles_for("The Show", Some("Pilot"), None, None, 0), ("The Show".into(), "Pilot".into()));
        assert_eq!(titles_for("Film", None, None, None, 1999), ("Film".into(), "1999".into()));
        assert_eq!(titles_for("Film", None, None, None, 0), ("Film".into(), String::new()));
    }
}
