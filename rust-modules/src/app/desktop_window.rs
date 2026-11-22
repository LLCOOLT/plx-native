//! The desktop window's events, taken before the loop decodes them: a resize re-fits the blit, the
//! fullscreen shortcut toggles fullscreen, and a mouse's extra buttons mean BACK or nothing. The SDL
//! half (the calls and the pure key and button tables) is the shell crate's
//! (`plx_shell_sdl::desktop`), the one SDL seam; this is only the application's reading of the raw
//! event. `hostsim` builds only: a television owns its whole panel and has one pointer button.
use plx_shell_sdl::desktop::{self, Button};
use plx_shell_sdl::event::{SDL_KEYDOWN, SDL_MOUSEBUTTONDOWN, SDL_MOUSEBUTTONUP, SYNTH_WINDOW};

use super::App;

/// `SDLK_ESCAPE`: the key a back mouse button stands for (the remote's BACK on the simulator).
const SDLK_ESCAPE: u32 = 27;

fn rd_u32(ev: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([ev[at], ev[at + 1], ev[at + 2], ev[at + 3]])
}

/// `true` when the event was the window's and nothing else may read it. A resize answers `false`:
/// it re-fits the blit and then still reaches the loop as page damage.
pub(crate) unsafe fn intercept(app: &mut App) -> bool {
    let et = rd_u32(&app.ev, 0);
    if et == desktop::SDL_WINDOWEVENT {
        plx_base::surface::window_resized(app.surface.window());
        return false;
    }
    // The fullscreen shortcut belongs to the window, not to the interface. Real first presses only,
    // never a synthetic one or an auto-repeat.
    if et == SDL_KEYDOWN
        && rd_u32(&app.ev, 8) != SYNTH_WINDOW
        && app.ev[13] == 0
        && desktop::toggles_fullscreen(rd_u32(&app.ev, 20), u16::from_ne_bytes([app.ev[24], app.ev[25]]))
    {
        desktop::toggle_fullscreen(app.surface.window());
        return true;
    }
    // The primary button is the remote's click; the right and the back side-button are the remote's
    // BACK (pushed as ESC, on the press); the others mean nothing here, and must not click either.
    if matches!(et, SDL_MOUSEBUTTONDOWN | SDL_MOUSEBUTTONUP) {
        match desktop::mouse_button(app.ev[16]) {
            Button::Primary => {}
            Button::Back => {
                if et == SDL_MOUSEBUTTONDOWN {
                    plx_shell_sdl::inject::synth_key(SDLK_ESCAPE, 0);
                }
                return true;
            }
            Button::Ignored => return true,
        }
    }
    false
}
