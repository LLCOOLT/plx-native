//! **The desktop pointer chrome**: the on-screen buttons a mouse needs and a remote does not.
//!
//! A television remote carries BACK, play/pause, the transport keys and the volume rocker as
//! hardware, so the interface never drew any of them. A desktop mouse has one button that means
//! anything here, so on the desktop builds those actions get faces: a back arrow and search and
//! settings shortcuts on pushed pages (this module, drawn by the dispatcher above the page), the
//! shelf paging arrows and a card's hover play/overflow buttons (`cards`), and the player's
//! transport row (`appkit::player_hud`). Every one of them reads [`PointerChrome`].
//!
//! **Two conditions, both required, and the television meets neither.** The chrome is ENABLED only
//! by the desktop port at boot ([`Desk::enable`] — nothing on webOS calls it), and it is LIVE only
//! while the pointer is the input in use: it has moved at least once and no D-pad press has taken
//! over since (`hit::HitMap::pointer_at`, the same gate that suppresses hover). A keyboard user on
//! a desktop therefore sees exactly the television's interface, and the first arrow key withdraws
//! every one of these buttons until the mouse travels again.
//!
//! The global buttons are not page elements: their keys live in their own namespace
//! ([`DESK_BASE`]), their stops never park focus (`Hover::Ignore`), and a click on one is taken by
//! the dispatcher's ingest before any page sees it and queued as a [`DeskAction`] for the
//! application, which performs it through the same paths the remote's keys and the tab strip use.
use crate::icons::Icon;
use crate::widgets::{CircleButton, ControlGround};
use crate::{Painter, Rect};

/// The pointer, as the draw sees it: where it is, while the desktop pointer chrome is live.
/// `Default` is "not live" — the television, a keyboard-driven desktop, every host test.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerChrome {
    pub live: bool,
    pub x: f32,
    pub y: f32,
}

impl PointerChrome {
    /// Live at `(x, y)`.
    pub const fn at(x: f32, y: f32) -> Self {
        Self { live: true, x, y }
    }

    /// Is the live pointer over `r` (screen space)? Always false when the chrome is not live, so a
    /// caller that asks only this cannot draw a hover state on the television.
    pub fn over(&self, r: Rect) -> bool {
        self.live && r.contains(self.x, self.y)
    }
}

/// The global chrome's key namespace: `DESK_BASE + action`, below the strip's and far above any
/// widget index (`geom::NAMESPACE_FLOOR`).
pub const DESK_BASE: u32 = 0xFFFE_0000;
const _: () = assert!(DESK_BASE >= crate::geom::NAMESPACE_FLOOR);
const _: () = assert!(DESK_BASE + 0xFFFF < crate::dispatch::STRIP_BASE);

/// What a global chrome button asks the application to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeskAction {
    /// The remote's BACK.
    Back,
    /// The tab strip's Search destination.
    Search,
    /// The Settings surface.
    Settings,
}

impl DeskAction {
    const ALL: [DeskAction; 3] = [DeskAction::Back, DeskAction::Search, DeskAction::Settings];

    pub const fn elem(self) -> u32 {
        DESK_BASE
            + match self {
                DeskAction::Back => 0,
                DeskAction::Search => 1,
                DeskAction::Settings => 2,
            }
    }

    /// The action a key names, if it is one of this namespace's.
    pub fn of_elem(elem: u32) -> Option<DeskAction> {
        Self::ALL.into_iter().find(|a| a.elem() == elem)
    }

    pub fn icon(self) -> Icon {
        match self {
            DeskAction::Back => Icon::ChevronLeft,
            DeskAction::Search => Icon::Search,
            DeskAction::Settings => Icon::Settings,
        }
    }
}

/// The disc every desktop chrome button is drawn as — a `CircleButton`, the same face as the
/// detail hero's discs, so the desktop's additions are the app's own material and not a second one.
pub const DISC: f32 = 56.0;
/// The global buttons' inset from the panel's corner. Deliberately OUTSIDE `consts::SAFE`: the safe
/// area is a television's overscan allowance, which a desktop window does not have, and the page's
/// own content already fills the safe frame — inside it these would sit on the hero's title.
pub const INSET: f32 = 28.0;
const GAP: f32 = 16.0;

/// Where each global button sits: BACK in the top-left corner, Search and Settings in the top-right.
pub fn place(action: DeskAction) -> Rect {
    use crate::consts::SCR_W;
    match action {
        DeskAction::Back => Rect::new(INSET, INSET, DISC, DISC),
        DeskAction::Search => Rect::new(SCR_W - INSET - 2.0 * DISC - GAP, INSET, DISC, DISC),
        DeskAction::Settings => Rect::new(SCR_W - INSET - DISC, INSET, DISC, DISC),
    }
}

/// One desktop chrome disc: `icon` centred on the shared control face, its hover state drawn as the
/// face's focus. `ground` is the surface under it (`Unkeyed` over video).
pub fn draw_button(p: Painter, r: Rect, icon: Icon, hovered: bool, ground: ControlGround) {
    use crate::View;
    CircleButton::new(c"".as_ptr())
        .frame(r)
        .icon(icon)
        .focused(hovered)
        .ground(ground)
        .draw(&crate::Env::inert(), p);
}

/// A card's hover play button: centred on the card, the disc a television would never draw.
pub const CARD_PLAY: f32 = 64.0;
/// A card's hover overflow button, in its bottom-right corner.
pub const CARD_MORE: f32 = 44.0;
const CARD_INSET: f32 = 10.0;

/// Where a hovered card's two buttons sit, from the card's drawn rect (any space; the result is in
/// the same one): the play disc centred, the overflow disc in the bottom-right corner.
pub fn card_buttons(card: Rect) -> (Rect, Rect) {
    let play = Rect::new(card.x + (card.w - CARD_PLAY) * 0.5, card.y + (card.h - CARD_PLAY) * 0.5, CARD_PLAY, CARD_PLAY);
    let more = Rect::new(card.x + card.w - CARD_INSET - CARD_MORE, card.y + card.h - CARD_INSET - CARD_MORE,
        CARD_MORE, CARD_MORE);
    (play, more)
}

/// A shelf's paging arrows, in SCREEN space, for a strip whose tiles span `top..top + h`: one disc
/// in each side margin, centred on the tiles.
pub fn shelf_arrows(top: f32, h: f32) -> (Rect, Rect) {
    let y = top + (h - DISC) * 0.5;
    (Rect::new(INSET, y, DISC, DISC), Rect::new(crate::consts::SCR_W - INSET - DISC, y, DISC, DISC))
}

/// Which card a shelf's arrow pages to: a whole screenful of cards on, from the run fully on
/// screen (`first..=last`) — the card a page away, which the strip's minimal reveal then brings
/// to the edge the arrow points at, so the shelf moves by exactly the cards that were showing.
/// `None` where there is nothing further that way (that arrow is not drawn).
pub fn page_target(first: usize, last: usize, len: usize, forward: bool) -> Option<usize> {
    let count = last.saturating_sub(first) + 1;
    if forward {
        (last + 1 < len).then(|| (last + count).min(len - 1))
    } else {
        (first > 0).then(|| first.saturating_sub(count))
    }
}

/// `r` (screen space) in painter `p`'s space, for a stop registered through `p`.
pub fn local(p: Painter, r: Rect) -> Rect {
    Rect::new(r.x - p.dx(), r.y - p.dy(), r.w, r.h)
}

/// The dispatcher's half: whether the chrome is enabled at all, and the actions clicked this frame
/// that the application has not taken yet.
#[derive(Default)]
pub struct Desk {
    enabled: bool,
    pending: Vec<DeskAction>,
}

impl Desk {
    /// The desktop port's one call. Nothing on the television makes it.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The pointer as the draw should see it: live only when enabled and the hit map has a pointer.
    pub fn read(&self, pointer_at: Option<(f32, f32)>) -> PointerChrome {
        match pointer_at {
            Some((x, y)) if self.enabled => PointerChrome::at(x, y),
            _ => PointerChrome::default(),
        }
    }

    /// The buttons a page with `wants` shows: all three on a pushed page, nothing otherwise.
    pub fn buttons(wants: bool) -> &'static [DeskAction] {
        if wants { &DeskAction::ALL } else { &[] }
    }

    pub(crate) fn push(&mut self, action: DeskAction) {
        self.pending.push(action);
    }

    /// What the application performs after the frame — in click order.
    pub fn take(&mut self) -> Vec<DeskAction> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chrome_is_never_live_until_the_desktop_port_enables_it() {
        let mut desk = Desk::default();
        assert_eq!(desk.read(Some((10.0, 10.0))), PointerChrome::default());
        assert!(!desk.read(Some((10.0, 10.0))).over(place(DeskAction::Back)));
        desk.enable();
        assert!(desk.read(Some((40.0, 40.0))).over(place(DeskAction::Back)));
        // a session driven by keys has no pointer, enabled or not
        assert!(!desk.read(None).live);
    }

    #[test]
    fn every_action_round_trips_through_its_key_and_no_other_key_is_one() {
        for a in DeskAction::ALL {
            assert_eq!(DeskAction::of_elem(a.elem()), Some(a));
        }
        assert_eq!(DeskAction::of_elem(DESK_BASE + 3), None);
        assert_eq!(DeskAction::of_elem(crate::dispatch::STRIP_BASE), None);
        assert_eq!(DeskAction::of_elem(0), None);
    }

    #[test]
    fn the_three_buttons_sit_on_the_panel_and_do_not_overlap() {
        let rects: Vec<Rect> = DeskAction::ALL.into_iter().map(place).collect();
        for (i, a) in rects.iter().enumerate() {
            assert!(a.x >= 0.0 && a.y >= 0.0 && a.x + a.w <= crate::consts::SCR_W);
            for b in &rects[i + 1..] {
                assert!(a.intersect(*b).w <= 0.0 || a.intersect(*b).h <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn an_arrow_pages_by_the_cards_that_were_showing_and_stops_at_the_ends() {
        // cards 3..=7 fully on screen of 20
        assert_eq!(page_target(3, 7, 20, true), Some(12));
        assert_eq!(page_target(3, 7, 20, false), Some(0));
        assert_eq!(page_target(8, 12, 20, false), Some(3));
        // the last page clamps to the last card, and there is no arrow past either end
        assert_eq!(page_target(14, 18, 20, true), Some(19));
        assert_eq!(page_target(15, 19, 20, true), None);
        assert_eq!(page_target(0, 4, 20, false), None);
        // a strip that fits has neither arrow
        assert_eq!(page_target(0, 3, 4, true), None);
        assert_eq!(page_target(0, 3, 4, false), None);
    }

    #[test]
    fn a_cards_buttons_sit_inside_it_without_overlapping() {
        let card = Rect::new(100.0, 200.0, 240.0, 360.0);
        let (play, more) = card_buttons(card);
        for r in [play, more] {
            assert!(r.x >= card.x && r.y >= card.y && r.x + r.w <= card.x + card.w && r.y + r.h <= card.y + card.h);
        }
        assert!(play.intersect(more).w <= 0.0 || play.intersect(more).h <= 0.0);
    }

    #[test]
    fn clicked_actions_are_handed_over_once_in_order() {
        let mut desk = Desk::default();
        desk.push(DeskAction::Search);
        desk.push(DeskAction::Back);
        assert_eq!(desk.take(), vec![DeskAction::Search, DeskAction::Back]);
        assert!(desk.take().is_empty());
    }
}
