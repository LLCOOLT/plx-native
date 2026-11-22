use super::*;

const MIN: i64 = 60_000;

fn ep(show: &str, season: i64, index: i64) -> Program {
    Program {
        rk: format!("{show}-{season}-{index}"),
        episode: true,
        title: format!("{show} {season}x{index}"),
        show_title: show.to_owned(),
        show_rk: show.to_owned(),
        season,
        index,
        dur_ms: 22 * MIN,
        ..Default::default()
    }
}

fn film(name: &str) -> Program {
    Program { rk: name.to_owned(), title: name.to_owned(), dur_ms: 100 * MIN, ..Default::default() }
}

fn recipe(source: Source, style: Style) -> Recipe {
    Recipe { name: "Sitcoms".into(), number: 900, source, style, seed: 42, ..Recipe::default() }
}

fn matched(p: &Program) -> Matched {
    Matched {
        rk: p.rk.clone(),
        id: format!("t-{}", p.rk),
        show_id: p.episode.then(|| format!("show-{}", p.show_rk)),
        show_rk: if p.episode { p.show_rk.clone() } else { String::new() },
        season: p.season,
        dur_ms: p.dur_ms,
    }
}

fn two_shows() -> Vec<Program> {
    vec![ep("A", 1, 1), ep("A", 1, 2), ep("B", 2, 1), ep("A", 1, 3), ep("B", 2, 2)]
}

#[test]
fn rotation_and_blocks_over_films_are_gaps_and_random_over_films_is_not() {
    let progs = vec![ep("A", 1, 1), film("F")];
    for style in [Style::RoundRobin, Style::Blocks] {
        assert_eq!(gaps(&recipe(Source::Library, style), &progs), vec![Gap::StyleNeedsShows(style)]);
    }
    assert!(gaps(&recipe(Source::Library, Style::Random), &progs).is_empty());
    assert!(gaps(&recipe(Source::Library, Style::InOrder), &progs).is_empty());
    assert!(gaps(&recipe(Source::Library, Style::RoundRobin), &two_shows()).is_empty());
}

#[test]
fn rules_tunarr_cannot_follow_are_gaps_never_dropped() {
    let mut r = recipe(Source::Library, Style::Random);
    r.rules.unwatched_only = true;
    r.rules.added_within_days = 30;
    assert_eq!(gaps(&r, &two_shows()), vec![Gap::UnwatchedOnly, Gap::AddedRecently]);
}

#[test]
fn too_many_shows_for_a_slot_each_is_a_gap() {
    let progs: Vec<Program> = (0..=MAX_SLOTS).map(|i| ep(&format!("S{i}"), 1, 1)).collect();
    assert_eq!(gaps(&recipe(Source::Library, Style::RoundRobin), &progs), vec![Gap::TooManyShows]);
    assert!(gaps(&recipe(Source::Library, Style::Random), &progs).is_empty());
}

#[test]
fn whole_shows_only_when_no_rule_narrows_the_show() {
    assert!(whole_shows(&recipe(Source::Show { rk: "A".into() }, Style::Random)));
    assert!(whole_shows(&recipe(Source::Season { rk: "A1".into() }, Style::InOrder)));
    let mut narrowed = recipe(Source::Show { rk: "A".into() }, Style::Random);
    narrowed.rules.min_minutes = 20;
    assert!(!whole_shows(&narrowed));
    let mut picked = recipe(Source::Library, Style::RoundRobin);
    picked.rules.shows = vec!["A".into(), "B".into()];
    assert!(whole_shows(&picked));
    picked.rules.genres = vec!["Comedy".into()];
    assert!(!whole_shows(&picked), "a genre narrows the picked shows");
    assert!(!whole_shows(&recipe(Source::Library, Style::Random)));
    assert!(!whole_shows(&recipe(Source::Playlist { rk: "p".into() }, Style::Random)));
}

#[test]
fn a_season_is_a_show_slot_filtered_to_the_season() {
    let progs = vec![ep("A", 3, 1), ep("A", 3, 2)];
    let m: Vec<Matched> = progs.iter().map(matched).collect();
    let slots = plan(&recipe(Source::Season { rk: "A3".into() }, Style::InOrder), &m);
    assert_eq!(slots, vec![Slot::Show { show_id: "show-A".into(), seasons: vec![3], weight: 2 }]);
}

#[test]
fn random_over_picked_shows_is_a_weighted_show_slot_each() {
    let mut r = recipe(Source::Library, Style::Random);
    r.rules.shows = vec!["A".into(), "B".into()];
    let m: Vec<Matched> = two_shows().iter().map(matched).collect();
    let slots = plan(&r, &m);
    assert_eq!(
        slots,
        vec![
            Slot::Show { show_id: "show-A".into(), seasons: vec![], weight: 3 },
            Slot::Show { show_id: "show-B".into(), seasons: vec![], weight: 2 },
        ]
    );
    let v = schedule_json(Style::Random, &slots, &HashMap::new(), 1);
    assert_eq!(v["randomDistribution"], "weighted");
    assert_eq!(v["slots"][0]["order"], "shuffle");
    assert_eq!(v["slots"][0]["type"], "show");
    assert_eq!(v["slots"][1]["weight"], 2);
}

#[test]
fn rotation_over_a_narrowed_library_is_a_custom_show_per_show_taken_in_turn() {
    let m: Vec<Matched> = two_shows().iter().map(matched).collect();
    let slots = plan(&recipe(Source::Library, Style::RoundRobin), &m);
    assert_eq!(
        slots,
        vec![
            Slot::List { key: "show:A".into(), programs: vec!["t-A-1-1".into(), "t-A-1-2".into(), "t-A-1-3".into()], weight: 3 },
            Slot::List { key: "show:B".into(), programs: vec!["t-B-2-1".into(), "t-B-2-2".into()], weight: 2 },
        ]
    );
    let lists: HashMap<String, String> = [("show:A".into(), "cs-a".into()), ("show:B".into(), "cs-b".into())].into();
    let v = schedule_json(Style::RoundRobin, &slots, &lists, 1);
    assert_eq!(v["randomDistribution"], "none", "slots in turn");
    assert_eq!(v["slots"][0]["order"], "next");
    assert_eq!(v["slots"][0]["customShowId"], "cs-a");
    assert_eq!(v["slots"][1]["customShowId"], "cs-b");
    assert_eq!(v["slots"][0]["durationSpec"]["programCount"], 1);
}

#[test]
fn blocks_air_several_episodes_a_turn() {
    let m: Vec<Matched> = two_shows().iter().map(matched).collect();
    let slots = plan(&recipe(Source::Library, Style::Blocks), &m);
    let v = schedule_json(Style::Blocks, &slots, &HashMap::new(), 1);
    assert_eq!(v["slots"][0]["durationSpec"]["programCount"], BLOCK);
    assert_eq!(v["randomDistribution"], "weighted");
}

#[test]
fn random_and_in_order_over_anything_else_are_one_list_in_the_preview_order() {
    let progs = vec![film("F2"), ep("A", 1, 1), film("F1")];
    let m: Vec<Matched> = progs.iter().map(matched).collect();
    for style in [Style::Random, Style::InOrder] {
        let slots = plan(&recipe(Source::Collection { rk: "c".into() }, style), &m);
        assert_eq!(slots, vec![Slot::List { key: "all".into(), programs: vec!["t-F2".into(), "t-A-1-1".into(), "t-F1".into()], weight: 3 }]);
    }
    let v = schedule_json(Style::InOrder, &plan(&recipe(Source::Library, Style::InOrder), &m), &HashMap::new(), 1);
    assert_eq!(v["slots"][0]["order"], "next");
    assert_eq!(v["randomDistribution"], "uniform");
    assert_eq!(v["maxDays"], CYCLE_DAYS);
}

#[test]
fn slot_ids_are_stable_well_formed_v4_uuids() {
    let a = slot_uuid(7, 0);
    assert_eq!(a, slot_uuid(7, 0));
    assert_ne!(a, slot_uuid(7, 1));
    assert_ne!(a, slot_uuid(8, 0));
    let parts: Vec<&str> = a.split('-').collect();
    assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
    assert!(parts[2].starts_with('4'));
    assert!(matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b')));
    assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
}

#[test]
fn programming_rebuilds_each_cycle_with_the_seed_as_two_words() {
    let v = programming_json(&["p1".into()], json!({}), 0x1_0000_0002);
    assert_eq!(v["type"], "random");
    assert_eq!(v["rebuildEachCycle"], true);
    assert_eq!(v["seed"], json!([2, 1]));
    assert_eq!(v["programs"], json!(["p1"]));
}

#[test]
fn a_free_number_is_kept_and_a_taken_one_moves_to_the_first_free() {
    assert_eq!(pick_number(905, &[1, 2, 900]), 905);
    assert_eq!(pick_number(900, &[900, 901, 903]), 902);
    assert_eq!(pick_number(0, &[]), FIRST_NUMBER);
}

#[test]
fn the_media_source_is_the_plex_server_by_identifier_and_tokens_are_not_read() {
    let body = br#"[
        {"type":"jellyfin","id":"jf","clientIdentifier":"m1"},
        {"type":"plex","id":"px-1","clientIdentifier":"other","accessToken":"secret"},
        {"type":"plex","id":"px-2","clientIdentifier":"m1","accessToken":"secret"}
    ]"#;
    assert_eq!(parse_media_source(body, "m1").as_deref(), Some("px-2"));
    assert_eq!(parse_media_source(body, "nope"), None, "two Plex sources and no match: ambiguous");
    let one = br#"[{"type":"plex","id":"only"}]"#;
    assert_eq!(parse_media_source(one, "m9").as_deref(), Some("only"));
    assert_eq!(parse_media_source(b"<html>", "m1"), None);
}

#[test]
fn lookups_map_back_to_rating_keys_and_unscanned_ones_are_counted() {
    let body = br#"{"u1":{"uuid":"u1","externalId":"A-1-1","showId":"s-A","duration":1300000},
                    "u2":{"uuid":"u2","externalId":"F","duration":0},
                    "bad":{"title":"no external id"}}"#;
    let found = parse_lookup(body);
    assert_eq!(found.len(), 2);
    let progs = vec![ep("A", 1, 1), film("F"), ep("A", 1, 2), ep("A", 1, 1)];
    let (m, missing) = match_programmes(&progs, &found);
    assert_eq!(m.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["u1", "u2"]);
    assert_eq!(m[0].show_id.as_deref(), Some("s-A"));
    assert_eq!(m[0].dur_ms, 1_300_000);
    assert_eq!(m[1].dur_ms, 100 * MIN, "a zero duration falls back to the library's");
    assert_eq!(missing, 1, "the duplicate is not counted twice");
    assert!(parse_lookup(b"[]").is_empty());
    assert_eq!(lookup_body("ms", &["a", "b"]), json!({"externalIds": ["plex|ms|a", "plex|ms|b"]}));
}

#[test]
fn small_parsers() {
    assert_eq!(parse_transcode_default(br#"[{"id":"a","isDefault":false},{"id":"b","isDefault":true}]"#).as_deref(), Some("b"));
    assert_eq!(parse_transcode_default(br#"[{"id":"a"}]"#).as_deref(), Some("a"));
    assert_eq!(parse_transcode_default(b"[]"), None);
    assert_eq!(parse_numbers(br#"[{"number":1},{"number":900},{"name":"x"}]"#), vec![1, 900]);
    assert_eq!(parse_id(br#"{"id":"c1"}"#).as_deref(), Some("c1"));
    assert_eq!(parse_id(br#"{"id":""}"#), None);
}

/// A scripted Tunarr: answers by `VERB path`, records every call.
#[derive(Default)]
struct Fake {
    calls: Vec<(String, String, Option<Value>)>,
    fail: Option<&'static str>,
    status: Option<u16>,
}

impl Http for Fake {
    fn call(&mut self, verb: &str, path: &str, body: Option<&Value>) -> Option<(u16, Vec<u8>)> {
        let key = format!("{verb} {path}");
        self.calls.push((verb.to_owned(), path.to_owned(), body.cloned()));
        if let Some(s) = self.status {
            return Some((s, Vec::new()));
        }
        if self.fail.is_some_and(|f| key.starts_with(f)) {
            return Some((500, Vec::new()));
        }
        let ok = |b: &str| Some((200, b.as_bytes().to_vec()));
        match key.as_str() {
            "GET /api/media-sources" => ok(r#"[{"type":"plex","id":"ms","clientIdentifier":"mach"}]"#),
            "POST /api/programming/batch/lookup" => {
                let ids = body.unwrap()["externalIds"].as_array().unwrap().clone();
                let mut out = serde_json::Map::new();
                for id in ids {
                    let rk = id.as_str().unwrap().rsplit('|').next().unwrap().to_owned();
                    if rk.starts_with("B") {
                        continue; // Tunarr has not scanned show B
                    }
                    let show = rk.split('-').next().unwrap();
                    out.insert(format!("t-{rk}"), json!({"uuid": format!("t-{rk}"), "externalId": rk, "showId": format!("show-{show}"), "duration": 1000}));
                }
                Some((200, serde_json::to_vec(&Value::Object(out)).unwrap()))
            }
            "GET /api/transcode_configs" => ok(r#"[{"id":"tc","isDefault":true}]"#),
            "GET /api/channels" => ok(r#"[{"number":900}]"#),
            "POST /api/custom-shows" => {
                let n = self.calls.iter().filter(|c| c.1 == "/api/custom-shows").count();
                Some((201, format!(r#"{{"id":"cs-{n}"}}"#).into_bytes()))
            }
            "POST /api/channels" => Some((201, br#"{"id":"ch-1"}"#.to_vec())),
            k if k.starts_with("POST /api/channels/") && k.ends_with("/programming") => ok("{}"),
            k if k.starts_with("DELETE ") => ok(""),
            _ => Some((404, Vec::new())),
        }
    }
}

#[test]
fn keeping_makes_the_list_the_channel_and_its_rebuilding_programming() {
    let mut http = Fake::default();
    let progs = vec![ep("A", 1, 1), ep("B", 1, 1), film("F")];
    let r = recipe(Source::Library, Style::Random);
    let kept = keep(&mut http, "http://tunarr:8000", &r, &progs, "mach", 1_000).unwrap();
    assert_eq!(kept.channel_id, "ch-1");
    assert_eq!(kept.number, 901, "900 was taken");
    assert_eq!(kept.recipe.number, 901);
    assert_eq!(kept.missing, 1, "show B is not in Tunarr");
    assert_eq!(kept.lists.get("all").map(String::as_str), Some("cs-1"));
    let lookup = http.calls.iter().find(|c| c.1 == "/api/programming/batch/lookup").unwrap();
    assert_eq!(lookup.2.as_ref().unwrap()["externalIds"][0], "plex|ms|A-1-1");
    let create = http.calls.iter().find(|c| c.0 == "POST" && c.1 == "/api/channels").unwrap();
    assert_eq!(create.2.as_ref().unwrap()["channel"]["transcodeConfigId"], "tc");
    assert_eq!(create.2.as_ref().unwrap()["channel"]["groupTitle"], GROUP_TITLE);
    let prog = http.calls.iter().find(|c| c.1 == "/api/channels/ch-1/programming").unwrap();
    let body = prog.2.as_ref().unwrap();
    assert_eq!(body["rebuildEachCycle"], true);
    assert_eq!(body["programs"], json!(["t-A-1-1", "t-F"]));
    assert_eq!(body["schedule"]["slots"][0]["customShowId"], "cs-1");
}

#[test]
fn a_failed_programming_write_removes_what_was_made() {
    let mut http = Fake { fail: Some("POST /api/channels/ch-1/programming"), ..Fake::default() };
    let progs = vec![ep("A", 1, 1), film("F")];
    let err = keep(&mut http, "o", &recipe(Source::Library, Style::Random), &progs, "mach", 1).unwrap_err();
    assert_eq!(err, KeepError::Failed);
    let deletes: Vec<&str> = http.calls.iter().filter(|c| c.0 == "DELETE").map(|c| c.1.as_str()).collect();
    assert_eq!(deletes, vec!["/api/channels/ch-1", "/api/custom-shows/cs-1"]);
}

#[test]
fn keeping_stops_before_any_write_on_a_gap_a_password_or_nothing_scanned() {
    let mut http = Fake::default();
    let mut r = recipe(Source::Library, Style::Random);
    r.rules.unwatched_only = true;
    assert_eq!(keep(&mut http, "o", &r, &[film("F")], "mach", 1), Err(KeepError::Gaps(vec![Gap::UnwatchedOnly])));
    assert!(http.calls.is_empty());

    let mut http = Fake { status: Some(401), ..Fake::default() };
    assert_eq!(keep(&mut http, "o", &recipe(Source::Library, Style::Random), &[film("F")], "m", 1), Err(KeepError::NeedsPassword));

    let mut http = Fake::default();
    assert_eq!(keep(&mut http, "o", &recipe(Source::Library, Style::Random), &[ep("B", 1, 1)], "mach", 1), Err(KeepError::NothingInTunarr));
    assert!(!http.calls.iter().any(|c| c.0 == "POST" && c.1 == "/api/channels"), "no channel was made");
    assert_eq!(keep(&mut Fake::default(), "o", &recipe(Source::Library, Style::Random), &[], "mach", 1), Err(KeepError::Empty));
}

#[test]
fn reordering_makes_the_new_lists_rewrites_and_drops_the_old_list() {
    let mut http = Fake::default();
    let progs = vec![ep("A", 1, 1), ep("A", 1, 2), ep("C", 1, 1)];
    let kept = keep(&mut http, "o", &recipe(Source::Library, Style::Random), &progs, "mach", 1).unwrap();
    let mut r = kept.recipe.clone();
    r.style = Style::RoundRobin;
    let mut http = Fake::default();
    let next = apply(&mut http, &kept, &r).unwrap();
    assert_eq!(next.lists.keys().cloned().collect::<Vec<_>>(), vec!["show:A".to_owned(), "show:C".to_owned()]);
    assert!(http.calls.iter().any(|c| c.0 == "DELETE" && c.1 == format!("/api/custom-shows/{}", kept.lists["all"])), "the old list goes");
    let prog = http.calls.iter().find(|c| c.1.ends_with("/programming")).unwrap();
    assert_eq!(prog.2.as_ref().unwrap()["schedule"]["randomDistribution"], "none");

    // Reshuffle: same lists, new seed, nothing made or deleted.
    let mut r2 = next.recipe.clone();
    r2.seed = 99;
    let mut http = Fake::default();
    let again = apply(&mut http, &next, &r2).unwrap();
    assert_eq!(again.lists, next.lists);
    assert!(http.calls.iter().all(|c| c.1.ends_with("/programming")));
}

#[test]
fn deleting_removes_the_channel_and_its_lists_and_a_vanished_channel_counts() {
    let kept = Kept {
        origin: "o".into(),
        channel_id: "ch".into(),
        number: 900,
        recipe: recipe(Source::Library, Style::Random),
        programs: vec![],
        lists: [("all".to_owned(), "cs".to_owned())].into(),
        missing: 0,
    };
    let mut http = Fake::default();
    assert_eq!(delete(&mut http, &kept), Ok(()));
    assert_eq!(http.calls.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), vec!["/api/channels/ch", "/api/custom-shows/cs"]);
    let mut gone = Fake { status: Some(404), ..Fake::default() };
    assert_eq!(delete(&mut gone, &kept), Ok(()));
    let mut locked = Fake { status: Some(401), ..Fake::default() };
    assert_eq!(delete(&mut locked, &kept), Err(KeepError::NeedsPassword));
}

#[test]
fn records_round_trip_and_list_by_server() {
    let k = |id: &str, origin: &str, n: u32| Kept {
        origin: origin.into(),
        channel_id: id.into(),
        number: n,
        recipe: recipe(Source::Library, Style::Random),
        programs: vec![],
        lists: BTreeMap::new(),
        missing: 0,
    };
    let mut r = Records::default();
    r.kept.insert("b".into(), k("b", "o1", 902));
    r.kept.insert("a".into(), k("a", "o1", 901));
    r.kept.insert("c".into(), k("c", "o2", 900));
    let round = Records::parse(&serde_json::to_string(&r).unwrap());
    assert_eq!(round, r);
    assert_eq!(r.on("o1").iter().map(|k| k.number).collect::<Vec<_>>(), vec![901, 902]);
    assert!(Records::parse("garbage").kept.is_empty());
}
