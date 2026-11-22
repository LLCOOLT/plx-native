//! Home's own shelves ([`HomeExtras`] and each source's genre and playlist rows): where the merge
//! places them, what it keeps off them, and that the ring and the deck never mistake them for a
//! server's.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

fn channel(rk: &str) -> Arc<PmsMovie> {
    Arc::new(PmsMovie { sid: ServerId::UNSET, kind: KIND_CHANNEL, rk: rk.into(), title: rk.into(), ..Default::default() })
}

fn ids(hubs: &[HubRow]) -> Vec<&str> {
    hubs.iter().map(|h| h.hub_id.as_str()).collect()
}

fn keys(items: &[Arc<PmsMovie>], hub: &HubRow) -> Vec<String> {
    items[hub.start..hub.start + hub.len].iter().map(|m| m.rk.clone()).collect()
}

fn with_genres(mut b: SourceBuild, slot: u16, rks: &[&str]) -> SourceBuild {
    b.genres = Some(rks.iter().map(|r| row(slot, r)).collect());
    b.genres_at_ms = Some(5);
    b
}

#[test]
fn the_app_shelves_follow_the_deck_in_a_fixed_order() {
    let srcs = [src(0, "", HubState::Ready, Some(with_genres(
        built(0, &[(9, "cw")], vec![shelf(0, "Recent", "home.movies.recent", &["r1"])]), 0, &["g1"])))];
    let extras = HomeExtras { on_now: vec![channel("12")], ..Default::default() };
    let (_, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &extras);
    assert_eq!(ids(&hubs), ["home.continue", ON_NOW_HUB, GENRES_HUB, "home.movies.recent"]);
    assert!(hubs.iter().all(|h| h.source.is_empty()), "an app shelf credits nobody");
}

#[test]
fn suggested_channels_follow_on_now_and_are_never_dropped_as_unpinned() {
    let srcs = [src(0, "", HubState::Ready, Some(built(0, &[], vec![shelf(0, "Recent", "home.movies.recent", &["r1"])])))];
    let idea = Arc::new(PmsMovie { sid: ServerId::UNSET, kind: KIND_CHANNEL_IDEA, rk: "genre:comedy".into(), title: "Comedy".into(), ..Default::default() });
    let extras = HomeExtras { on_now: vec![channel("12")], channels: vec![idea] };
    let (cat, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &extras);
    assert_eq!(ids(&hubs), [ON_NOW_HUB, CHANNELS_HUB, "home.movies.recent"]);
    assert_eq!(keys(&cat, &hubs[1]), ["genre:comedy"]);
    assert!(!item_has_menu_kind(KIND_CHANNEL_IDEA), "a suggestion has no item menu: OK opens the studio");
}

#[test]
fn an_empty_app_shelf_draws_no_heading() {
    let srcs = [src(0, "", HubState::Ready, Some(built(0, &[], vec![shelf(0, "Recent", "home.movies.recent", &["r1"])])))];
    let (_, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &HomeExtras::default());
    assert_eq!(ids(&hubs), ["home.movies.recent"]);
}

#[test]
fn the_genre_shelf_shows_only_what_no_other_shelf_does() {
    let srcs = [src(0, "", HubState::Ready, Some(with_genres(
        built(0, &[(9, "cw")], vec![shelf(0, "Recent", "home.movies.recent", &["r1"])]),
        0, &["cw", "r1", "fresh"])))];
    let (items, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &HomeExtras::default());
    let genres = hubs.iter().find(|h| h.hub_id == GENRES_HUB).expect("the genre shelf");
    assert_eq!(keys(&items, genres), ["fresh"], "the deck and the server shelves already show the rest");
}

#[test]
fn the_genre_shelf_takes_turns_between_sources() {
    let srcs = [
        src(0, "", HubState::Ready, Some(with_genres(built(0, &[], Vec::new()), 0, &["a1", "a2", "a3"]))),
        src(1, "friend", HubState::Ready, Some(with_genres(built(1, &[], Vec::new()), 1, &["b1"]))),
    ];
    let (items, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &HomeExtras::default());
    assert_eq!(keys(&items, &hubs[0]), ["a1", "b1", "a2", "a3"]);
}

#[test]
fn an_unpinned_librarys_titles_stay_off_the_app_shelves_but_channels_do_not() {
    let mut g = (*row(0, "g1")).clone();
    g.sec = 3;
    let mut b = built(0, &[], Vec::new());
    b.genres = Some(vec![Arc::new(g)]);
    let srcs = [src(0, "", HubState::Ready, Some(b))];
    let extras = HomeExtras { on_now: vec![channel("5")], ..Default::default() };
    let pins = [(sid(0), 3, false)];
    let scope = BrowseScope { sections_gen: 0, pins: &pins, clients: std::borrow::Cow::Owned(registry_keys_for_test()) };
    let (_, hubs, _) = merge_with_scope(&srcs, &scope, &extras);
    assert_eq!(ids(&hubs), [ON_NOW_HUB], "a channel belongs to no library");
}

#[test]
fn an_app_shelf_keeps_one_identity_whichever_server_leads_it() {
    let srcs: [Src; 0] = [];
    for lead in [0, 1] {
        let card = |slot: u16, rk: &str| Arc::new(PmsMovie { sid: sid(slot), kind: KIND_CHANNEL, rk: rk.into(), ..Default::default() });
        let extras = HomeExtras { on_now: vec![card(lead, "w"), card(1 - lead, "v")], ..Default::default() };
        let (items, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &extras);
        assert_eq!(
            stable_hub_identity(&hubs[0], &items),
            Some(HubIdentity::Identifier { sid: ServerId::UNSET, id: ON_NOW_HUB, key: "" }),
        );
    }
}

/// The ring counts every row before the servers' as leading, so the app shelves sit inside it
/// and are never given up to a descriptor.
#[test]
fn the_ring_reads_the_app_shelves_as_leading_rows() {
    let srcs = [src(0, "", HubState::Ready, Some(built(0, &[(1, "cw")], vec![shelf(0, "Recent", "home.movies.recent", &["r1"])])))];
    let extras = HomeExtras { on_now: vec![channel("1"), channel("2")], ..Default::default() };
    let (_, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &extras);
    let order = published_order(&srcs, &[]);
    assert_eq!(hubs.len() - order.len(), 2, "the deck and On Now lead the server shelves");
    assert_eq!(hubs[2].hub_id, "home.movies.recent");
}

#[test]
fn an_unchanged_on_now_shelf_does_not_republish_home() {
    let _g = plx_base::testlock::serial();
    let mut owner = Owner::default();
    seed(&mut owner.state, vec![src(0, "", HubState::Ready, Some(built(0, &[], Vec::new())))]);
    let card = |title: &str| PmsMovie { kind: KIND_CHANNEL, rk: "4".into(), title: title.into(), ..Default::default() };
    let scope = BrowseScope::standalone();
    fn on_now(e: &mut HomeExtras) -> &mut Vec<Arc<PmsMovie>> { &mut e.on_now }
    assert!(set_extra(&mut owner.state, &scope, vec![card("news")], on_now), "a new shelf publishes");
    let gen = owner.state.catalog_gen;
    assert!(!set_extra(&mut owner.state, &scope, vec![card("news")], on_now), "the same cards again: nothing to publish");
    assert_eq!(owner.state.catalog_gen, gen);
    assert!(set_extra(&mut owner.state, &scope, vec![card("film")], on_now), "the programme changed");
    assert_eq!(hub_title(&owner.state, 0), plx_os::i18n::msg::browse_home_on_now());
    assert!(set_extra(&mut owner.state, &scope, Vec::new(), on_now));
    assert_eq!(hub_count(&owner.state), 0, "Live TV turned off: the shelf goes");
}

#[test]
fn a_fetch_that_did_not_build_the_genre_shelf_keeps_the_last_one() {
    let mut s = src(0, "", HubState::Ready, None);
    landed_ok(&mut s, with_genres(built(0, &[], Vec::new()), 0, &["g1"]));
    landed_ok(&mut s, built(0, &[], Vec::new()));
    let last = s.last.as_ref().unwrap();
    let kept: Vec<_> = last.genres.as_ref().unwrap().iter().map(|m| m.rk.clone()).collect();
    assert_eq!(kept, ["g1"], "a plain refresh keeps the genre rows");
    assert_eq!(last.genres_at_ms, Some(5), "and the shelf's own clock");
    landed_ok(&mut s, with_genres(built(0, &[], Vec::new()), 0, &[]));
    assert!(s.last.as_ref().unwrap().genres.as_ref().unwrap().is_empty(), "a rebuilt empty shelf replaces it");
}

#[test]
fn a_servers_video_playlists_become_playlist_cards() {
    let mc: plx_plex::plex::MediaContainer = serde_json::from_str(r#"{"Metadata":[
        {"type":"playlist","ratingKey":"77","title":"s77","composite":"/playlists/77/composite/1","leafCount":4,"playlistType":"video"},
        {"type":"playlist","ratingKey":"78","title":"empty","leafCount":0},
        {"type":"movie","ratingKey":"1","title":"not a playlist","leafCount":3}]}"#).unwrap();
    let rows = playlist_rows(&mc, sid(0));
    assert_eq!(rows.len(), 1, "an empty playlist and a non-playlist row are left off");
    assert_eq!((rows[0].kind, rows[0].rk.as_str(), rows[0].thumb.as_str(), rows[0].child_count),
        (KIND_PLAYLIST, "77", "/playlists/77/composite/1", 4));
    assert!(item_has_menu_kind(KIND_PLAYLIST), "a playlist's hold offers Make a Channel");
}

#[test]
fn the_playlists_shelf_follows_the_genre_shelf() {
    let mut b = with_genres(built(0, &[], vec![shelf(0, "Recent", "home.movies.recent", &["r1"])]), 0, &["g1"]);
    b.playlists = Some(vec![Arc::new(PmsMovie { kind: KIND_PLAYLIST, rk: "77".into(), title: "s77".into(), ..Default::default() })]);
    let srcs = [src(0, "", HubState::Ready, Some(b))];
    let (_, hubs, _) = merge_with_scope(&srcs, &BrowseScope::standalone(), &HomeExtras::default());
    assert_eq!(ids(&hubs), [GENRES_HUB, PLAYLISTS_HUB, "home.movies.recent"]);
}
