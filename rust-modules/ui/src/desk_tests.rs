//! The desktop pointer chrome (`ui::desk`) through the real dispatcher: when its global buttons
//! exist at all, that a click on one is the application's and never the page's, and the three click
//! kinds a card's own buttons carry (`screen::Activate::{Focus, Hold, Play}`).

use super::desk::{DeskAction, DESK_BASE};
use super::dispatch::{Dispatcher, NoTap};
use super::fixture::{events_of, tick, FixtureArg, FixtureHost, FixtureRig};
use super::screen::{Activate, Hover, Stop};
use super::Rect;
use plx_machine::machine::{Edge, FocusKey, InputEvent, InputKind, Key, MachineId, NavOp, Source, Tick};

fn boot(arg: FixtureArg, desktop: bool) -> (Dispatcher<FixtureHost>, FixtureRig) {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    if desktop {
        d.desk.enable();
    }
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(arg));
    d.frame(&mut rig, tick(0), vec![], vec![], &mut NoTap);
    (d, rig)
}

fn ev(kind: InputKind<u32>, at: Tick) -> InputEvent<u32> {
    InputEvent { at, source: Source::Sdl, kind }
}

fn pointer(x: f32, y: f32, at: Tick) -> InputEvent<u32> {
    ev(InputKind::Pointer { x, y, hit: None }, at)
}

fn click(x: f32, y: f32, at: Tick) -> InputEvent<u32> {
    ev(InputKind::Click { x, y, hit: None }, at)
}

fn key(k: Key, edge: Edge, at: Tick) -> InputEvent<u32> {
    ev(InputKind::Key { key: k, sym: 0, wcode: 0, edge, at_edge: false }, at)
}

fn desk_stops(d: &Dispatcher<FixtureHost>) -> Vec<u32> {
    d.input.hit.front().iter().map(|s| s.key.elem).filter(|e| DeskAction::of_elem(*e).is_some()).collect()
}

fn centre(action: DeskAction) -> (f32, f32) {
    let r = super::desk::place(action);
    (r.cx(), r.cy())
}

/// The television's case: the chrome is never enabled, so however the pointer moves, no global
/// button is drawn or clickable.
#[test]
fn without_the_desktop_port_no_global_button_ever_exists() {
    let (mut d, mut rig) = boot(FixtureArg::Page(1), false);
    d.frame(&mut rig, tick(16), vec![pointer(900.0, 500.0, tick(16))], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    assert!(desk_stops(&d).is_empty());
    let (x, y) = centre(DeskAction::Back);
    d.frame(&mut rig, tick(32), vec![click(x, y, tick(32))], vec![], &mut NoTap);
    assert!(d.desk.take().is_empty());
}

/// Enabled, the buttons wait for the pointer: a desktop driven by keys sees the television's
/// interface, the first pointer move brings them, and a D-pad press withdraws them again.
#[test]
fn the_global_buttons_follow_the_pointer_and_a_dpad_press_withdraws_them() {
    let (mut d, mut rig) = boot(FixtureArg::Page(1), true);
    d.draw(&mut rig, true);
    assert!(desk_stops(&d).is_empty(), "no pointer has moved yet");
    d.frame(&mut rig, tick(16), vec![pointer(900.0, 500.0, tick(16))], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    let mut stops = desk_stops(&d);
    stops.sort();
    assert_eq!(stops, vec![DESK_BASE, DESK_BASE + 1, DESK_BASE + 2], "back, search and settings");
    assert!(d.input.hit.front().iter().filter(|s| DeskAction::of_elem(s.key.elem).is_some())
        .all(|s| s.hover == Hover::Ignore), "a global button never parks focus");
    d.frame(&mut rig, tick(32), vec![key(Key::Down, Edge::Down, tick(32))], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    assert!(desk_stops(&d).is_empty(), "a D-pad press hands the screen back to the keys");
}

/// A page that does not ask for them (a tab-bar root, which has the strip) never gets them.
#[test]
fn only_a_page_that_asks_wears_the_global_buttons() {
    let (mut d, mut rig) = boot(FixtureArg::Home, true);
    d.frame(&mut rig, tick(16), vec![pointer(900.0, 500.0, tick(16))], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    assert!(desk_stops(&d).is_empty());
}

/// A click on a global button is queued for the application and reaches the page as nothing at
/// all — no activation, no input — so no page can mistake the button for one of its elements.
#[test]
fn a_global_buttons_click_is_the_applications_and_never_the_pages() {
    let (mut d, mut rig) = boot(FixtureArg::Page(1), true);
    d.frame(&mut rig, tick(16), vec![pointer(900.0, 500.0, tick(16))], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    let before = events_of(&d, 0);
    let (x, y) = centre(DeskAction::Search);
    d.frame(&mut rig, tick(32), vec![click(x, y, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.desk.take(), vec![DeskAction::Search]);
    let after = events_of(&d, 0);
    let new = &after[before.len().min(after.len())..];
    assert!(!new.contains("\"activate\"") && !new.contains("\"input\""), "the page saw the click: {new}");
}

/// A full-screen surface that asks (Settings, in the app) wears the back arrow alone, keyed to the
/// surface — the input owner — and its click is the application's BACK like any other.
#[test]
fn an_open_surface_that_asks_wears_the_back_arrow_alone() {
    use super::containers::modal::Style;
    let (mut d, mut rig) = boot(FixtureArg::Home, true);
    d.nav.next_style = Style::Opaque { snapshot: true };
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(&mut rig, tick(16), vec![pointer(900.0, 500.0, tick(16))], vec![], &mut NoTap);
    for ms in (32..=1600).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    d.draw(&mut rig, true);
    let surface = d.nav.modals.surfaces.last().expect("the modal is up").entry.id;
    let back: Vec<_> = d.input.hit.front().iter().filter(|s| DeskAction::of_elem(s.key.elem).is_some()).collect();
    assert_eq!(back.len(), 1, "the back arrow alone");
    assert_eq!(back[0].key, FocusKey { entry: surface, elem: DeskAction::Back.elem() });
    let (x, y) = centre(DeskAction::Back);
    d.frame(&mut rig, tick(1616), vec![click(x, y, tick(1616))], vec![], &mut NoTap);
    assert_eq!(d.desk.take(), vec![DeskAction::Back]);
}

/// One injected stop over the fixture page's first card, carrying `activate`, keyed by that card.
fn card_button(d: &mut Dispatcher<FixtureHost>, rig: &mut FixtureRig, activate: Activate) -> FocusKey<u32> {
    d.frame(rig, tick(8), vec![pointer(5.0, 5.0, tick(8))], vec![], &mut NoTap);
    d.draw(rig, true);
    let card = d.input.hit.front().first().copied().expect("the fixture page registers a card");
    let r = Rect::new(1700.0, 900.0, 44.0, 44.0);
    let mut stops = d.input.hit.front().to_vec();
    stops.push(Stop { key: card.key, rect: r, rest_rect: r, clip: Rect::FULL, hover: Hover::Ignore, activate });
    d.input.hit.fill(stops);
    d.input.hit.swap();
    card.key
}

/// The overflow button is a HOLD on its card, delivered at once — so a fast click, its button-down
/// and -up in one frame, still opens the menu and never also activates the card.
#[test]
fn the_overflow_button_holds_its_card_even_on_a_same_frame_click() {
    let (mut d, mut rig) = boot(FixtureArg::Page(700), true);
    card_button(&mut d, &mut rig, Activate::Hold);
    d.frame(&mut rig, tick(16), vec![click(1720.0, 920.0, tick(16)), key(Key::Ok, Edge::Up, tick(16))], vec![], &mut NoTap);
    for ms in (32..=960).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    let events = events_of(&d, 0);
    assert_eq!(events.matches("\"press_hold\"").count(), 1, "{events}");
    assert!(!events.contains("\"press_commit\""), "the button never taps the card: {events}");
}

/// The play button asks the page to play its card, and is nothing else: no hold, no press.
#[test]
fn the_play_button_asks_the_page_to_play_its_card() {
    let (mut d, mut rig) = boot(FixtureArg::Page(700), true);
    card_button(&mut d, &mut rig, Activate::Play);
    d.frame(&mut rig, tick(16), vec![click(1720.0, 920.0, tick(16)), key(Key::Ok, Edge::Up, tick(16))], vec![], &mut NoTap);
    for ms in (32..=640).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    let events = events_of(&d, 0);
    assert_eq!(events.matches("\"play_card\"").count(), 1, "{events}");
    assert!(!events.contains("\"press_hold\"") && !events.contains("\"press_commit\""), "{events}");
    assert!(d.input.arm.is_none());
}

/// A paging arrow moves focus to the card it is keyed by — as a pointer move does — and arms no
/// press, so the card it lands on is not opened.
#[test]
fn a_paging_arrow_moves_focus_and_activates_nothing() {
    let (mut d, mut rig) = boot(FixtureArg::Page(700), true);
    d.frame(&mut rig, tick(4), vec![], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    let cards: Vec<_> = d.input.hit.front().iter().filter(|s| s.activate == Activate::Press).copied().collect();
    let target = cards.last().expect("the fixture page registers cards").key;
    let r = Rect::new(1700.0, 900.0, 44.0, 44.0);
    let mut stops = d.input.hit.front().to_vec();
    stops.push(Stop { key: target, rect: r, rest_rect: r, clip: Rect::FULL, hover: Hover::Ignore, activate: Activate::Focus });
    d.input.hit.fill(stops);
    d.input.hit.swap();
    d.frame(&mut rig, tick(16), vec![click(1720.0, 920.0, tick(16)), key(Key::Ok, Edge::Up, tick(16))], vec![], &mut NoTap);
    for ms in (32..=640).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.focus(), Some(target));
    let events = events_of(&d, 0);
    assert!(!events.contains("\"press_commit\"") && !events.contains("\"activate\""), "{events}");
}
