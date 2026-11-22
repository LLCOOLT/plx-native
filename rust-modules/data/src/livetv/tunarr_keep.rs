//! **Keeping a channel in Tunarr**: the television as Tunarr's remote. A draft made in the studio
//! (a suggestion, *Surprise me*, *Make a Channel* on a title, *New Channel*) can be kept as a REAL
//! Tunarr channel instead of a Plex playlist, so it airs for every client Tunarr serves — Plex DVR,
//! other apps, this one through native direct play (`super::native`).
//!
//! **How a recipe becomes a Tunarr channel.** The app already knows exactly which programmes the
//! recipe selects: the studio's preview built them from the library with every rule applied
//! (`crate::vchannel::build`). Those Plex items are looked up in Tunarr by their ratingKey
//! (`POST /api/programming/batch/lookup`, ids `plex|<media source>|<ratingKey>`), and the channel is
//! made of what Tunarr holds:
//!
//! | Recipe | Tunarr programming (random slots, *Rebuild each cycle* on) |
//! |---|---|
//! | a show or a season, no other rule | one `show` slot per show (a season sets its `seasonFilter`): new episodes join |
//! | anything else | the programmes as one Tunarr custom show (a fixed list) |
//! | style Random | `shuffle` order (shows weighted by their episode counts) |
//! | style In order | `next` order over the source's own order |
//! | style Rotation | one slot per show, `next` order, slots taken in turn (`randomDistribution: none`) |
//! | style Blocks | one slot per show, `next` order, [`BLOCK`] episodes a turn, shows weighted |
//!
//! A slot per show is a Tunarr `show` slot when the recipe takes whole shows, else a custom show
//! of that show's selected episodes. What cannot be carried is a [`Gap`], and the studio says so
//! instead of keeping a channel that would not be what the preview showed: Rotation and Blocks
//! over films (Tunarr's slots rotate shows), "only unwatched" (Tunarr cannot follow what this
//! profile watches) and "added recently" (a fixed list would keep titles after they stop being
//! recent). A programme Tunarr has not scanned yet cannot be scheduled there; the channel is made
//! of the rest and the count left out is reported ([`Kept::missing`]).
//!
//! **What the television remembers.** Tunarr has no field for a recipe, so the record of each kept
//! channel — its recipe, its Tunarr channel and custom-show ids and the programmes it was made of —
//! is kept on this television ([`Records`], `plx_base::paths::tunarr_kept_candidates`). Reshuffle
//! (a new seed) and Order (a new style) rewrite the channel's programming from it; Delete removes
//! the channel and the custom shows made for it. The channel itself is in Tunarr's guide on every
//! client; another television sees it there but cannot reshuffle it.
//!
//! **Access.** Tunarr's API needs no credential unless its owner set `TUNARR_BASIC_AUTH_*`; a 401
//! is [`KeepError::NeedsPassword`] (the studio then offers keeping it in this app). Nothing here
//! reads a Plex token: the media-source list is read only for its `id`, `type` and
//! `clientIdentifier`, and its body is dropped at once.
//!
//! Pure apart from [`keep`], [`apply`] and [`delete`], which BLOCK on a worker.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::vchannel::recipe::{Kinds, Recipe, Rules, Source};
use crate::vchannel::schedule::{mix, Program, Style};

/// The guide group Tunarr files these channels under.
pub const GROUP_TITLE: &str = "PlxNative";
/// Episodes a Blocks turn airs.
pub const BLOCK: u32 = 3;
/// Slots a channel holds at most (one per show): past this, Rotation and Blocks are a [`Gap`].
pub const MAX_SLOTS: usize = 48;
/// External ids per lookup request.
pub const LOOKUP_CHUNK: usize = 200;
/// A rebuilding random-slot channel's cycle: a fresh order every week.
pub const CYCLE_DAYS: u32 = 7;
/// The number Tunarr channels kept from here start at, as virtual ones do.
pub const FIRST_NUMBER: u32 = crate::vchannel::recipe::FIRST_NUMBER;

/// Something the recipe asks that Tunarr's programming cannot carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// Rotation or Blocks over a channel that holds films.
    StyleNeedsShows(Style),
    /// More shows than [`MAX_SLOTS`] for a slot per show.
    TooManyShows,
    UnwatchedOnly,
    AddedRecently,
}

/// What [`gaps`] finds in `recipe` over the `programmes` its preview built. Empty: Tunarr can
/// carry it.
pub fn gaps(recipe: &Recipe, programmes: &[Program]) -> Vec<Gap> {
    let mut out = Vec::new();
    let per_show = matches!(recipe.style, Style::RoundRobin | Style::Blocks);
    if per_show && programmes.iter().any(|p| !p.episode || p.show_rk.is_empty()) {
        out.push(Gap::StyleNeedsShows(recipe.style));
    }
    if per_show {
        let mut shows: Vec<&str> = programmes.iter().filter(|p| p.episode).map(|p| p.show_rk.as_str()).collect();
        shows.sort_unstable();
        shows.dedup();
        if shows.len() > MAX_SLOTS {
            out.push(Gap::TooManyShows);
        }
    }
    if recipe.rules.unwatched_only {
        out.push(Gap::UnwatchedOnly);
    }
    if recipe.rules.added_within_days > 0 {
        out.push(Gap::AddedRecently);
    }
    out
}

/// Does the recipe take whole shows, so Tunarr `show` slots (which follow the show as new episodes
/// are scanned) mean exactly what it selects? A show or a season with no narrowing rule, or a
/// library channel of picked shows and nothing else.
pub fn whole_shows(recipe: &Recipe) -> bool {
    let only = |r: &Rules| {
        let bare = Rules { kinds: r.kinds, shows: r.shows.clone(), ..Rules::default() };
        *r == bare && r.kinds != Kinds::Movies
    };
    match &recipe.source {
        Source::Show { .. } | Source::Season { .. } => only(&recipe.rules),
        Source::Library => !recipe.rules.shows.is_empty() && recipe.rules.films.is_empty() && only(&recipe.rules),
        Source::Playlist { .. } | Source::Collection { .. } => false,
    }
}

/// A programme as Tunarr holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Matched {
    /// The Plex ratingKey it was looked up by.
    pub rk: String,
    /// Tunarr's program uuid.
    pub id: String,
    /// Tunarr's show uuid, for an episode.
    #[serde(default)]
    pub show_id: Option<String>,
    /// The Plex show ratingKey, for an episode (the group the styles keep together).
    #[serde(default)]
    pub show_rk: String,
    #[serde(default)]
    pub season: i64,
    pub dur_ms: i64,
}

/// One slot of the channel's programming.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Slot {
    /// A Tunarr show, whole (or the listed seasons of it).
    Show { show_id: String, seasons: Vec<i64>, weight: usize },
    /// A list of programmes, made into a Tunarr custom show; `key` names its membership so a
    /// rewrite can reuse one already made.
    List { key: String, programs: Vec<String>, weight: usize },
}

impl Slot {
    fn weight(&self) -> usize {
        match self {
            Slot::Show { weight, .. } | Slot::List { weight, .. } => *weight,
        }
    }
}

/// The slots `recipe` airs as, from the programmes Tunarr holds (in the preview's order).
pub fn plan(recipe: &Recipe, matched: &[Matched]) -> Vec<Slot> {
    let per_show = matches!(recipe.style, Style::RoundRobin | Style::Blocks);
    let shows_whole = whole_shows(recipe) && matched.iter().all(|m| m.show_id.is_some());
    // The shows in first-seen order, each with its programmes.
    let mut order: Vec<String> = Vec::new();
    let mut by_show: HashMap<String, Vec<&Matched>> = HashMap::new();
    for m in matched {
        let key = if m.show_rk.is_empty() { m.rk.clone() } else { m.show_rk.clone() };
        if !by_show.contains_key(&key) {
            order.push(key.clone());
        }
        by_show.entry(key).or_default().push(m);
    }
    let whole = |key: &str| -> Slot {
        let ms = &by_show[key];
        let mut seasons: Vec<i64> = if matches!(recipe.source, Source::Season { .. }) {
            ms.iter().map(|m| m.season).collect()
        } else {
            Vec::new()
        };
        seasons.sort_unstable();
        seasons.dedup();
        Slot::Show { show_id: ms[0].show_id.clone().unwrap_or_default(), seasons, weight: ms.len() }
    };
    let listed = |key: &str| -> Slot {
        let ms = &by_show[key];
        Slot::List { key: format!("show:{key}"), programs: ms.iter().map(|m| m.id.clone()).collect(), weight: ms.len() }
    };
    if per_show {
        return order.iter().map(|k| if shows_whole { whole(k) } else { listed(k) }).collect();
    }
    match recipe.style {
        // Shuffling whole shows: one slot each, weighted by size, follows new episodes.
        Style::Random if shows_whole => order.iter().map(|k| whole(k)).collect(),
        // In order over one whole show is the show's own order.
        Style::InOrder if shows_whole && order.len() == 1 => vec![whole(&order[0])],
        _ => vec![Slot::List {
            key: "all".to_owned(),
            programs: matched.iter().map(|m| m.id.clone()).collect(),
            weight: matched.len(),
        }],
    }
}

/// Slot `i`'s id under `seed`: a well-formed version-4 uuid (Tunarr validates the shape), the same
/// for the same channel and slot so a rewrite keeps them.
pub fn slot_uuid(seed: u64, i: usize) -> String {
    let a = mix(seed, 0x5107_0000u64.wrapping_add(i as u64));
    let b = mix(a, 0x5eed);
    let hex = format!("{a:016x}{b:016x}");
    let h = hex.as_bytes();
    let s = |r: std::ops::Range<usize>| std::str::from_utf8(&h[r]).unwrap_or("0");
    let variant = ['8', '9', 'a', 'b'][(b & 3) as usize];
    format!("{}-{}-4{}-{}{}-{}", s(0..8), s(8..12), s(13..16), variant, s(17..20), s(20..32))
}

/// The random-slot schedule for `slots`, with the custom-show ids `lists` (by [`Slot::List`]
/// key) already made.
pub fn schedule_json(style: Style, slots: &[Slot], lists: &HashMap<String, String>, seed: u64) -> Value {
    let order = match style {
        Style::Random => "shuffle",
        Style::RoundRobin | Style::Blocks | Style::InOrder => "next",
    };
    let count = if style == Style::Blocks { BLOCK } else { 1 };
    let distribution = match style {
        Style::RoundRobin => "none",
        _ if slots.len() > 1 => "weighted",
        _ => "uniform",
    };
    let slots: Vec<Value> = slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let mut v = json!({
                "id": slot_uuid(seed, i),
                "order": order,
                "direction": "asc",
                "cooldownMs": 0,
                "weight": slot.weight().max(1),
                "durationSpec": { "type": "dynamic", "programCount": count },
            });
            match slot {
                Slot::Show { show_id, seasons, .. } => {
                    v["type"] = json!("show");
                    v["showId"] = json!(show_id);
                    v["seasonFilter"] = json!(seasons);
                    v["seasonExcludeFilter"] = json!([]);
                }
                Slot::List { key, .. } => {
                    v["type"] = json!("custom-show");
                    v["customShowId"] = json!(lists.get(key).cloned().unwrap_or_default());
                }
            }
            v
        })
        .collect();
    json!({
        "type": "random",
        "flexPreference": "end",
        "maxDays": CYCLE_DAYS,
        "padMs": 0,
        "padStyle": "episode",
        "slots": slots,
        "randomDistribution": distribution,
        "lockWeights": false,
    })
}

/// Tunarr's RNG takes 32-bit words.
pub fn seed_words(seed: u64) -> [u32; 2] {
    [seed as u32, (seed >> 32) as u32]
}

/// The body of `POST /api/channels/{id}/programming`.
pub fn programming_json(programs: &[String], schedule: Value, seed: u64) -> Value {
    json!({
        "type": "random",
        "programs": programs,
        "schedule": schedule,
        "seed": seed_words(seed),
        "rebuildEachCycle": true,
    })
}

/// The body of `POST /api/channels` for a new channel: Tunarr's own defaults, as its auto-channel
/// service writes them.
pub fn channel_json(id: &str, name: &str, number: u32, transcode_config_id: &str, now_ms: i64) -> Value {
    json!({
        "type": "new",
        "channel": {
            "id": id,
            "name": name,
            "number": number,
            "duration": 0,
            "startTime": now_ms,
            "groupTitle": GROUP_TITLE,
            "icon": { "path": "", "width": 0, "duration": 0, "position": "bottom-right" },
            "stealth": false,
            "disableFillerOverlay": false,
            "guideMinimumDuration": 30000,
            "offline": { "mode": "pic" },
            "streamMode": "hls",
            "transcodeConfigId": transcode_config_id,
            "subtitlesEnabled": false,
            "useEtvNext": false,
        }
    })
}

/// The body of `POST /api/custom-shows`.
pub fn custom_show_json(name: &str, programs: &[(String, i64)]) -> Value {
    let programs: Vec<Value> =
        programs.iter().map(|(id, dur)| json!({ "type": "content", "id": id, "duration": (*dur).max(1) })).collect();
    json!({
        "name": name,
        "programs": programs,
        "syncMediaSourceId": null,
        "syncMediaSourceType": null,
        "syncExternalPlaylistId": null,
    })
}

/// The channel number to take: `wanted` when free, else the first free one from [`FIRST_NUMBER`].
pub fn pick_number(wanted: u32, taken: &[u32]) -> u32 {
    if wanted > 0 && !taken.contains(&wanted) {
        return wanted;
    }
    (FIRST_NUMBER..).find(|n| !taken.contains(n)).unwrap_or(FIRST_NUMBER)
}

/// The Tunarr media source serving the Plex server `machine_id`: the one whose `clientIdentifier`
/// is it, else the only Plex source there is. Reads `id`, `type` and `clientIdentifier` only.
pub fn parse_media_source(body: &[u8], machine_id: &str) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let plex: Vec<(&str, Option<&str>)> = v
        .as_array()?
        .iter()
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("plex"))
        .filter_map(|s| Some((s.get("id")?.as_str()?, s.get("clientIdentifier").and_then(Value::as_str))))
        .collect();
    if let Some((id, _)) = plex.iter().find(|(_, c)| !machine_id.is_empty() && *c == Some(machine_id)) {
        return Some((*id).to_owned());
    }
    match plex.as_slice() {
        [(id, _)] => Some((*id).to_owned()),
        _ => None,
    }
}

/// The external ids `rks` are looked up by.
pub fn lookup_body(media_source: &str, rks: &[&str]) -> Value {
    json!({ "externalIds": rks.iter().map(|rk| format!("plex|{media_source}|{rk}")).collect::<Vec<_>>() })
}

/// A batch lookup's answer (uuid → program), by the Plex ratingKey each program carries.
pub fn parse_lookup(body: &[u8]) -> HashMap<String, (String, Option<String>, i64)> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return HashMap::new() };
    let Some(map) = v.as_object() else { return HashMap::new() };
    map.iter()
        .filter_map(|(uuid, p)| {
            let rk = p.get("externalId")?.as_str()?.to_owned();
            let id = p.get("uuid").and_then(Value::as_str).unwrap_or(uuid).to_owned();
            let show = p.get("showId").and_then(Value::as_str).map(str::to_owned);
            let dur = p.get("duration").and_then(Value::as_i64).unwrap_or(0);
            Some((rk, (id, show, dur)))
        })
        .collect()
}

/// The programmes Tunarr holds, in the preview's order, and how many it does not.
pub fn match_programmes(programmes: &[Program], found: &HashMap<String, (String, Option<String>, i64)>) -> (Vec<Matched>, usize) {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut missing = 0;
    for p in programmes {
        if !seen.insert(p.rk.as_str()) {
            continue;
        }
        match found.get(&p.rk) {
            Some((id, show, dur)) => out.push(Matched {
                rk: p.rk.clone(),
                id: id.clone(),
                show_id: if p.episode { show.clone() } else { None },
                show_rk: if p.episode { p.show_rk.clone() } else { String::new() },
                season: p.season,
                dur_ms: if *dur > 0 { *dur } else { p.dur_ms },
            }),
            None => missing += 1,
        }
    }
    (out, missing)
}

/// The default transcode config's id (or the first one).
pub fn parse_transcode_default(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let list = v.as_array()?;
    let pick = list.iter().find(|c| c.get("isDefault").and_then(Value::as_bool) == Some(true)).or(list.first())?;
    pick.get("id")?.as_str().map(str::to_owned)
}

/// The numbers Tunarr's channels hold.
pub fn parse_numbers(body: &[u8]) -> Vec<u32> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return Vec::new() };
    v.as_array()
        .map(|l| l.iter().filter_map(|c| c.get("number")?.as_u64()).map(|n| n as u32).collect())
        .unwrap_or_default()
}

/// The `id` of a created channel or custom show.
pub fn parse_id(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("id")?.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// A channel this television kept in Tunarr.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    /// The Tunarr server, as the Live TV setting names it.
    pub origin: String,
    pub channel_id: String,
    pub number: u32,
    pub recipe: Recipe,
    /// The programmes it was made of, as Tunarr holds them.
    pub programs: Vec<Matched>,
    /// The custom shows made for it, by [`Slot::List`] key.
    #[serde(default)]
    pub lists: BTreeMap<String, String>,
    /// Programmes of the preview Tunarr does not hold (not scanned yet), left out.
    #[serde(default)]
    pub missing: usize,
}

/// Why a keep (or a rewrite) did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeepError {
    /// The recipe asks what Tunarr cannot carry ([`gaps`]).
    Gaps(Vec<Gap>),
    /// Tunarr's API wants a password (basic auth is set on that server).
    NeedsPassword,
    /// Tunarr has no source for the Plex server the channel is drawn from.
    NoMediaSource,
    /// Tunarr holds none of the channel's programmes (the library is not scanned there).
    NothingInTunarr,
    /// The preview had no programmes.
    Empty,
    /// Tunarr could not be reached, or refused a write.
    Failed,
}

/// The transport [`keep`], [`apply`] and [`delete`] speak through: `(status, body)`, or `None`
/// when nothing answered. [`Live`] in production, a script in tests.
pub trait Http {
    fn call(&mut self, verb: &str, path: &str, body: Option<&Value>) -> Option<(u16, Vec<u8>)>;
}

/// Tunarr over libcurl (plain `http://` on the LAN, or `https://`).
pub struct Live {
    pub origin: String,
}

impl Http for Live {
    fn call(&mut self, verb: &str, path: &str, body: Option<&Value>) -> Option<(u16, Vec<u8>)> {
        let url = format!("{}{}", self.origin.trim_end_matches('/'), path);
        let bytes = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        let mut headers = vec!["Accept: application/json".to_owned()];
        if bytes.is_some() {
            headers.push("Content-Type: application/json".to_owned());
        }
        let reply = plx_net::net::request(&url, &headers, verb, bytes.as_deref(), plx_net::net::API, false, Some(32 << 20), None)?;
        Some((reply.status, reply.body))
    }
}

fn expect(http: &mut dyn Http, verb: &str, path: &str, body: Option<&Value>) -> Result<Vec<u8>, KeepError> {
    match http.call(verb, path, body) {
        Some((s, b)) if (200..300).contains(&s) => Ok(b),
        Some((401, _)) => Err(KeepError::NeedsPassword),
        _ => Err(KeepError::Failed),
    }
}

/// Make the custom shows `slots` need that `have` does not hold. Returns every list's id.
fn make_lists(
    http: &mut dyn Http,
    name: &str,
    slots: &[Slot],
    programs: &[Matched],
    have: &BTreeMap<String, String>,
    made: &mut Vec<String>,
) -> Result<BTreeMap<String, String>, KeepError> {
    let dur: HashMap<&str, i64> = programs.iter().map(|m| (m.id.as_str(), m.dur_ms)).collect();
    let mut lists = BTreeMap::new();
    for slot in slots {
        let Slot::List { key, programs: ids, .. } = slot else { continue };
        if let Some(id) = have.get(key) {
            lists.insert(key.clone(), id.clone());
            continue;
        }
        let title = if key == "all" { name.to_owned() } else { format!("{name} \u{b7} {}", lists.len() + 1) };
        let items: Vec<(String, i64)> = ids.iter().map(|id| (id.clone(), dur.get(id.as_str()).copied().unwrap_or(1))).collect();
        let body = expect(http, "POST", "/api/custom-shows", Some(&custom_show_json(&title, &items)))?;
        let id = parse_id(&body).ok_or(KeepError::Failed)?;
        made.push(id.clone());
        lists.insert(key.clone(), id);
    }
    Ok(lists)
}

fn write_programming(http: &mut dyn Http, channel_id: &str, style: Style, slots: &[Slot], lists: &BTreeMap<String, String>, programs: &[Matched], seed: u64) -> Result<(), KeepError> {
    let lists: HashMap<String, String> = lists.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let schedule = schedule_json(style, slots, &lists, seed);
    let ids: Vec<String> = programs.iter().map(|m| m.id.clone()).collect();
    expect(http, "POST", &format!("/api/channels/{channel_id}/programming"), Some(&programming_json(&ids, schedule, seed))).map(|_| ())
}

fn undo(http: &mut dyn Http, channel: Option<&str>, lists: &[String]) {
    if let Some(c) = channel {
        let _ = http.call("DELETE", &format!("/api/channels/{c}"), None);
    }
    for l in lists {
        let _ = http.call("DELETE", &format!("/api/custom-shows/{l}"), None);
    }
}

/// **Keep `recipe` as a Tunarr channel**, made of `programmes` (the preview's), drawn from the
/// Plex server `machine_id`. BLOCKS. Anything made before a failure is removed again.
pub fn keep(http: &mut dyn Http, origin: &str, recipe: &Recipe, programmes: &[Program], machine_id: &str, now_ms: i64) -> Result<Kept, KeepError> {
    let g = gaps(recipe, programmes);
    if !g.is_empty() {
        return Err(KeepError::Gaps(g));
    }
    if programmes.is_empty() {
        return Err(KeepError::Empty);
    }
    let sources = expect(http, "GET", "/api/media-sources", None)?;
    let media_source = parse_media_source(&sources, machine_id).ok_or(KeepError::NoMediaSource)?;
    drop(sources);
    let mut found = HashMap::new();
    let rks: Vec<&str> = programmes.iter().map(|p| p.rk.as_str()).collect();
    for chunk in rks.chunks(LOOKUP_CHUNK) {
        let body = expect(http, "POST", "/api/programming/batch/lookup", Some(&lookup_body(&media_source, chunk)))?;
        found.extend(parse_lookup(&body));
    }
    let (programs, missing) = match_programmes(programmes, &found);
    if programs.is_empty() {
        return Err(KeepError::NothingInTunarr);
    }
    let slots = plan(recipe, &programs);
    let transcode = parse_transcode_default(&expect(http, "GET", "/api/transcode_configs", None)?).ok_or(KeepError::Failed)?;
    let taken = parse_numbers(&expect(http, "GET", "/api/channels", None)?);
    let number = pick_number(recipe.number, &taken);
    let mut made = Vec::new();
    let lists = match make_lists(http, &recipe.name, &slots, &programs, &BTreeMap::new(), &mut made) {
        Ok(l) => l,
        Err(e) => {
            undo(http, None, &made);
            return Err(e);
        }
    };
    let id = slot_uuid(mix(recipe.seed, now_ms as u64), usize::MAX);
    let created = match expect(http, "POST", "/api/channels", Some(&channel_json(&id, &recipe.name, number, &transcode, now_ms))) {
        Ok(body) => parse_id(&body).unwrap_or(id),
        Err(e) => {
            undo(http, None, &made);
            return Err(e);
        }
    };
    if let Err(e) = write_programming(http, &created, recipe.style, &slots, &lists, &programs, recipe.seed) {
        undo(http, Some(&created), &made);
        return Err(e);
    }
    let mut kept = recipe.clone();
    kept.number = number;
    Ok(Kept { origin: origin.to_owned(), channel_id: created, number, recipe: kept, programs, lists, missing })
}

/// **Rewrite a kept channel's programming** under `recipe`'s style and seed (Reshuffle, Order).
/// BLOCKS. Custom shows the new slots need are made; ones they no longer use are deleted after
/// the write lands.
pub fn apply(http: &mut dyn Http, kept: &Kept, recipe: &Recipe) -> Result<Kept, KeepError> {
    let g = gaps(recipe, &[]).into_iter().chain(style_gaps(recipe.style, &kept.programs)).collect::<Vec<_>>();
    if !g.is_empty() {
        return Err(KeepError::Gaps(g));
    }
    let slots = plan(recipe, &kept.programs);
    let mut made = Vec::new();
    let lists = match make_lists(http, &recipe.name, &slots, &kept.programs, &kept.lists, &mut made) {
        Ok(l) => l,
        Err(e) => {
            undo(http, None, &made);
            return Err(e);
        }
    };
    if let Err(e) = write_programming(http, &kept.channel_id, recipe.style, &slots, &lists, &kept.programs, recipe.seed) {
        undo(http, None, &made);
        return Err(e);
    }
    for (key, id) in &kept.lists {
        if !lists.contains_key(key) {
            let _ = http.call("DELETE", &format!("/api/custom-shows/{id}"), None);
        }
    }
    Ok(Kept { recipe: recipe.clone(), lists, ..kept.clone() })
}

/// The per-show styles' gap over programmes already matched.
fn style_gaps(style: Style, programs: &[Matched]) -> Vec<Gap> {
    let per_show = matches!(style, Style::RoundRobin | Style::Blocks);
    let mut out = Vec::new();
    if per_show && programs.iter().any(|m| m.show_rk.is_empty()) {
        out.push(Gap::StyleNeedsShows(style));
    }
    if per_show {
        let mut shows: Vec<&str> = programs.iter().map(|m| m.show_rk.as_str()).collect();
        shows.sort_unstable();
        shows.dedup();
        if shows.len() > MAX_SLOTS {
            out.push(Gap::TooManyShows);
        }
    }
    out
}

/// **Delete a kept channel** and the custom shows made for it. BLOCKS. A channel Tunarr no longer
/// has (deleted in Tunarr's own UI) counts as deleted.
pub fn delete(http: &mut dyn Http, kept: &Kept) -> Result<(), KeepError> {
    match http.call("DELETE", &format!("/api/channels/{}", kept.channel_id), None) {
        Some((s, _)) if (200..300).contains(&s) || s == 404 => {}
        Some((401, _)) => return Err(KeepError::NeedsPassword),
        _ => return Err(KeepError::Failed),
    }
    for id in kept.lists.values() {
        let _ = http.call("DELETE", &format!("/api/custom-shows/{id}"), None);
    }
    Ok(())
}

/// The record of every channel this television kept in Tunarr, by channel id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Records {
    pub kept: BTreeMap<String, Kept>,
}

impl Records {
    pub fn parse(raw: &str) -> Records {
        serde_json::from_str(raw).unwrap_or_default()
    }

    /// The stored record, from the first candidate that reads. BLOCKS: a worker body (the
    /// [`Keeper`] asks for it as work).
    pub fn load() -> Records {
        plx_base::paths::tunarr_kept_candidates()
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|raw| Records::parse(&raw))
            .unwrap_or_default()
    }

    /// Write the record (tmp + rename on the first writable candidate). BLOCKS: a storage-worker
    /// body.
    pub fn write(body: &str) {
        for path in plx_base::paths::tunarr_kept_candidates() {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                return;
            }
        }
    }

    /// The channels kept in the Tunarr server `origin`, by number.
    pub fn on(&self, origin: &str) -> Vec<&Kept> {
        let mut v: Vec<&Kept> = self.kept.values().filter(|k| k.origin == origin).collect();
        v.sort_by_key(|k| k.number);
        v
    }
}

/// The command vocabulary (carried by `LiveTvCmd::Tunarr`).
#[derive(Clone, Debug, PartialEq)]
pub enum TunarrCmd {
    /// Keep the previewed draft `token` as a Tunarr channel.
    Keep { token: u64, recipe: Recipe },
    /// A new seed for a kept channel.
    Reshuffle { channel_id: String, seed: u64 },
    /// A new order style for a kept channel.
    Reorder { channel_id: String, style: Style },
    Delete { channel_id: String },
}

/// A write's outcome, kept for the page that asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunarrDone {
    Kept { token: u64, channel_id: String, number: u32, missing: usize },
    Updated { channel_id: String },
    Deleted { channel_id: String },
    /// `token` is the Keep's, 0 for a rewrite or a delete.
    Failed { token: u64, error: KeepError },
}

/// How many outcomes [`Keeper::done`] holds.
pub const DONE_KEPT: usize = 8;

type Landing = (TunarrDone, Option<Kept>, Option<String>);
type Job = std::sync::mpsc::Receiver<Landing>;

/// The channels this television kept in Tunarr, and the writes in flight. Owned by the Live TV
/// store; its IO leaves as work on its outbox (`crate::livetv::work`), and the answers come back
/// through channels drained by [`Keeper::pump`] on the frame thread.
#[derive(Default)]
pub struct Keeper {
    /// The stored record; `None` until its read lands.
    records: Option<Records>,
    /// The record's read, in flight.
    reading: Option<std::sync::mpsc::Receiver<Records>>,
    jobs: Vec<Job>,
    /// Writes that finished before the record was read: they land with it.
    early: Vec<Landing>,
    done: Vec<TunarrDone>,
    done_seq: u64,
    input: super::work::LiveTvInput,
    out: super::work::Outbox,
}

impl Keeper {
    /// Ask for the stored record, unless it is read or being read.
    fn read_records(&mut self) {
        if self.records.is_some() || self.reading.is_some() || self.input.controlled {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.out.worker("tunarr-kept-read", move |_| {
            let _ = tx.send(Records::load());
        });
        self.reading = Some(rx);
    }

    /// Write the record on the storage worker.
    fn save_records(&mut self) {
        let Some(r) = self.records.as_ref() else { return };
        if self.input.controlled {
            return;
        }
        let body = serde_json::to_string(r).unwrap_or_default();
        self.out.storage("tunarr-kept-save", move |_| Records::write(&body));
    }

    /// Take the input the app captured for this step.
    pub fn capture(&mut self, input: &super::work::LiveTvInput) {
        if self.input != *input {
            self.input = input.clone();
        }
    }

    /// The IO asked for since the last take.
    pub fn take_work(&mut self) -> Vec<super::work::Work> {
        self.out.take()
    }

    /// The channels kept in `origin`, by number (empty until the record is first read).
    pub fn on(&self, origin: &str) -> Vec<&Kept> {
        self.records.as_ref().map(|r| r.on(origin)).unwrap_or_default()
    }

    pub fn get(&self, channel_id: &str) -> Option<&Kept> {
        self.records.as_ref()?.kept.get(channel_id)
    }

    /// Ask for the record if it has not been read (the studio calls this when it opens). `true`
    /// when the read was asked for now.
    pub fn load(&mut self) -> bool {
        let fresh = self.records.is_none() && self.reading.is_none();
        self.read_records();
        fresh && self.reading.is_some()
    }

    pub fn busy(&self) -> bool {
        !self.jobs.is_empty() || self.reading.is_some()
    }

    /// The outcomes so far and how many there have been in all (see `Channels::done`).
    pub fn done(&self) -> (u64, &[TunarrDone]) {
        (self.done_seq, &self.done)
    }

    /// The outcomes past `seen`, oldest first.
    pub fn done_since(&self, seen: u64) -> &[TunarrDone] {
        let n = self.done_seq.saturating_sub(seen) as usize;
        &self.done[self.done.len().saturating_sub(n)..]
    }

    fn finish(&mut self, d: TunarrDone) {
        self.done.push(d);
        self.done_seq += 1;
        if self.done.len() > DONE_KEPT {
            self.done.remove(0);
        }
    }

    /// Ask for `work` on a worker. `false` under a controlled run, which asks for nothing.
    fn spawn(&mut self, name: &'static str, work: impl FnOnce(&dyn super::work::Reach) -> Landing + Send + 'static) -> bool {
        if self.input.controlled {
            return false;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.out.worker(name, move |reach| {
            let _ = tx.send(work(reach));
        });
        self.jobs.push(rx);
        true
    }

    /// Start `cmd` against the Tunarr server `origin`. `programmes` is the preview the Keep is of
    /// (none: the draft has not been built yet, and the Keep fails as [`KeepError::Empty`]).
    pub fn run(&mut self, cmd: TunarrCmd, origin: &str, programmes: Option<Vec<Program>>, now_ms: i64) -> bool {
        self.read_records();
        let origin = origin.to_owned();
        match cmd {
            TunarrCmd::Keep { token, recipe } => {
                let Some(programmes) = programmes.filter(|p| !p.is_empty()) else {
                    self.finish(TunarrDone::Failed { token, error: KeepError::Empty });
                    return true;
                };
                let sid = self.input.server;
                let started = self.spawn("tunarr-keep", move |reach| {
                    let machine = reach.client(sid).map(|c| c.machine_id().to_owned()).unwrap_or_default();
                    let mut http = Live { origin: origin.clone() };
                    match keep(&mut http, &origin, &recipe, &programmes, &machine, now_ms) {
                        Ok(k) => (
                            TunarrDone::Kept { token, channel_id: k.channel_id.clone(), number: k.number, missing: k.missing },
                            Some(k),
                            None,
                        ),
                        Err(error) => (TunarrDone::Failed { token, error }, None, None),
                    }
                });
                if !started {
                    self.finish(TunarrDone::Failed { token, error: KeepError::Failed });
                }
                true
            }
            TunarrCmd::Reshuffle { channel_id, seed } => self.rewrite(channel_id, |r| r.seed = seed),
            TunarrCmd::Reorder { channel_id, style } => self.rewrite(channel_id, |r| r.style = style),
            TunarrCmd::Delete { channel_id } => {
                let Some(kept) = self.get(&channel_id).cloned() else { return false };
                let started = self.spawn("tunarr-delete", move |_| {
                    let mut http = Live { origin: kept.origin.clone() };
                    match delete(&mut http, &kept) {
                        Ok(()) => (TunarrDone::Deleted { channel_id: kept.channel_id.clone() }, None, Some(kept.channel_id)),
                        Err(error) => (TunarrDone::Failed { token: 0, error }, None, None),
                    }
                });
                if !started {
                    self.finish(TunarrDone::Failed { token: 0, error: KeepError::Failed });
                }
                true
            }
        }
    }

    fn rewrite(&mut self, channel_id: String, change: impl FnOnce(&mut Recipe)) -> bool {
        let Some(kept) = self.get(&channel_id).cloned() else { return false };
        let mut recipe = kept.recipe.clone();
        change(&mut recipe);
        let started = self.spawn("tunarr-rewrite", move |_| {
            let mut http = Live { origin: kept.origin.clone() };
            match apply(&mut http, &kept, &recipe) {
                Ok(k) => (TunarrDone::Updated { channel_id: k.channel_id.clone() }, Some(k), None),
                Err(error) => (TunarrDone::Failed { token: 0, error }, None, None),
            }
        });
        if !started {
            self.finish(TunarrDone::Failed { token: 0, error: KeepError::Failed });
        }
        true
    }

    /// Drain the workers. Returns `(changed, guide_moved)`: the second asks the store to reload
    /// Tunarr's guide, so a kept or deleted channel shows (or goes) at once.
    pub fn pump(&mut self) -> (bool, bool) {
        let mut changed = false;
        let mut guide = false;
        if let Some(rx) = self.reading.as_ref() {
            match rx.try_recv() {
                Ok(r) => {
                    self.records = Some(r);
                    self.reading = None;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.records = Some(Records::default());
                    self.reading = None;
                    changed = true;
                }
            }
        }
        let mut landed = std::mem::take(&mut self.early);
        self.jobs.retain(|rx| match rx.try_recv() {
            Ok(out) => {
                landed.push(out);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                landed.push((TunarrDone::Failed { token: 0, error: KeepError::Failed }, None, None));
                false
            }
        });
        if self.records.is_none() {
            // The record is not read yet: a landing would replace it. Hold them until it is.
            if !landed.is_empty() {
                self.read_records();
            }
            self.early = landed;
            return (changed, guide);
        }
        for (done, kept, removed) in landed {
            let mut save = false;
            let records = self.records.get_or_insert_with(Records::default);
            if let Some(k) = kept {
                guide |= matches!(done, TunarrDone::Kept { .. });
                records.kept.insert(k.channel_id.clone(), k);
                save = true;
            }
            if let Some(id) = removed {
                records.kept.remove(&id);
                guide = true;
                save = true;
            }
            if save {
                self.save_records();
            }
            self.finish(done);
            changed = true;
        }
        (changed, guide)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(&mut self, kept: Vec<Kept>) {
        let r = self.records.get_or_insert_with(Records::default);
        for k in kept {
            r.kept.insert(k.channel_id.clone(), k);
        }
    }
}

#[cfg(test)]
#[path = "tunarr_keep_tests.rs"]
mod tests;
