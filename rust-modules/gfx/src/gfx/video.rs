//! **The desktop's decoded picture**: Y'CbCr planes in, one textured quad UNDER the finished UI out.
//!
//! A television never reaches this: its decoder writes a hardware video plane beneath the UI plane
//! and the compositor blends the two. The desktop has one GL surface, so the player's desktop sink
//! (`plx_media::player::desk`) hands its decoded frames here and the same arithmetic happens in a
//! shader: the UI plane holds premultiplied colour over a transparent clear, and the picture is
//! drawn with `(ONE_MINUS_DST_ALPHA, ONE)`, so the screen shows `ui.rgb + video.rgb * (1 - ui.a)`
//! exactly as the panel would. `draw_under` (the RGBA screenshot path) is the same composite.
//!
//! The planes go up as the decoder left them and are converted in `shaders/fs_video.frag`: an
//! RGB conversion on the CPU would cost a 4K frame's worth of arithmetic per picture on the
//! decode thread, which is the one place a desktop cannot afford it. Main-render-thread only, like
//! every GL call in this crate.
use std::os::raw::{c_int, c_uint, c_void};

use super::{
    glActiveTexture, glBindTexture, glBlendFuncSeparate, glDrawArrays, glGenTextures,
    glGetUniformLocation, glPixelStorei, glTexImage2D, glTexParameteri, glUniform1f, glUniform1i,
    glUniform2f, glUniform4f, gl_is_es, link_optional_program, tex_ledger, use_prog, GL_CLAMP_TO_EDGE,
    GL_LINEAR, GL_ONE, GL_ONE_MINUS_DST_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_RGBA, GL_SRC_ALPHA,
    GL_TEXTURE0, GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_TEXTURE_MIN_FILTER, GL_TEXTURE_WRAP_S,
    GL_TEXTURE_WRAP_T, GL_TRIANGLE_STRIP, GL_UNPACK_ALIGNMENT, GL_UNSIGNED_BYTE, SCR_H, SCR_W, VS_SRC,
};

extern "C" {
    fn glTexSubImage2D(
        target: c_uint,
        level: c_int,
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        format: c_uint,
        ty: c_uint,
        pixels: *const c_void,
    );
}

const FS_VIDEO: &std::ffi::CStr = glsl!("../shaders/fs_video.frag");

// Core-profile formats; a GLES2 driver gets the LUMINANCE pair instead (see `formats`).
const GL_UNPACK_ROW_LENGTH: c_uint = 0x0CF2;
const GL_RED: c_uint = 0x1903;
const GL_RG: c_uint = 0x8227;
const GL_R8: c_uint = 0x8229;
const GL_RG8: c_uint = 0x822B;
const GL_RGBA8: c_uint = 0x8058;
const GL_LUMINANCE: c_uint = 0x1909;
const GL_LUMINANCE_ALPHA: c_uint = 0x190A;

/// One plane as the decoder left it: `w`x`h` texels of `bytes` bytes each, rows `stride` bytes
/// apart. A 16-bit sample is two bytes of one texel, never a 16-bit texel (see the shader).
#[derive(Clone, Copy)]
pub struct Plane {
    pub data: *const u8,
    pub stride: c_int,
    pub w: c_int,
    pub h: c_int,
    pub bytes: c_int,
}

/// How a picture's codes become colour: what the shader's uniforms say, decided by the caller
/// from the stream's own description.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Chroma is one interleaved plane (NV12, P010) rather than two.
    pub interleaved: bool,
    /// Samples are 16-bit words stored as two bytes.
    pub wide: bool,
    /// The largest code a sample's container holds, the divisor that normalizes it.
    pub max: f32,
    /// `levels` of the stream's range and depth.
    pub levels: [f32; 4],
    /// (Kr, Kb) of the stream's matrix.
    pub k: [f32; 2],
    pub transfer: Transfer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Sdr,
    Pq,
    Hlg,
}

/// The luma and chroma offset and gain that take a normalized code (`code / max`) to `Y'` in
/// [0, 1] and `Cb`/`Cr` in [-0.5, 0.5]. `bits` is the depth the codes are written at — 16 for a
/// most-significant-aligned 10-bit format like P010, whose limited range is 64<<6..940<<6.
pub fn levels(full_range: bool, bits: u32, max: f32) -> [f32; 4] {
    let s = (1u64 << (bits - 8)) as f32;
    if full_range {
        let top = ((1u64 << bits) - 1) as f32;
        [0.0, max / top, 128.0 * s / max, max / top]
    } else {
        [16.0 * s / max, max / (219.0 * s), 128.0 * s / max, max / (224.0 * s)]
    }
}

/// (Kr, Kb) for an `AVColorSpace`: BT.709 for 1, BT.2020 non-constant for 9 and 10, BT.601 for 5
/// and 6. Anything unspecified follows the picture's size, as players and the standards' own
/// defaults do: SD is BT.601, HD BT.709.
pub fn matrix(colorspace: c_int, height: c_int) -> [f32; 2] {
    const BT601: [f32; 2] = [0.299, 0.114];
    const BT709: [f32; 2] = [0.2126, 0.0722];
    const BT2020: [f32; 2] = [0.2627, 0.0593];
    match colorspace {
        1 => BT709,
        5 | 6 => BT601,
        9 | 10 => BT2020,
        _ if height > 576 => BT709,
        _ => BT601,
    }
}

/// The textures one picture lives in, re-specified only when a plane's shape changes. Owned by
/// whoever draws (one per player), released with [`Textures::release`].
#[derive(Default)]
pub struct Textures {
    tex: [c_uint; 3],
    shape: [(c_int, c_int, c_int); 3],
}

struct Program {
    prog: c_uint,
    vid: c_int,
    nv: c_int,
    wide: c_int,
    hi_a: c_int,
    max: c_int,
    lev: c_int,
    k: c_int,
    hdr: c_int,
}

/// Linked on first use: a desktop that never plays a video never compiles it. `None` once a driver
/// refused it, which leaves the picture out and the UI as it is.
static mut PROGRAM: Option<Option<Program>> = None;

#[allow(static_mut_refs)]
fn program() -> Option<&'static Program> {
    unsafe {
        if PROGRAM.is_none() {
            let linked = link_optional_program(VS_SRC.as_ptr(), FS_VIDEO.as_ptr()).map(|prog| {
                let loc = |name: &std::ffi::CStr| glGetUniformLocation(prog, name.as_ptr());
                use_prog(prog);
                glUniform4f(loc(c"u_rect"), 0.0, 0.0, SCR_W, SCR_H);
                glUniform2f(loc(c"u_screen"), SCR_W, SCR_H);
                glUniform1i(loc(c"u_y"), 0);
                glUniform1i(loc(c"u_u"), 1);
                glUniform1i(loc(c"u_v"), 2);
                glUniform1f(loc(c"u_hi_a"), if gl_is_es() { 1.0 } else { 0.0 });
                Program {
                    prog,
                    vid: loc(c"u_vid"),
                    nv: loc(c"u_nv"),
                    wide: loc(c"u_wide"),
                    hi_a: loc(c"u_hi_a"),
                    max: loc(c"u_max"),
                    lev: loc(c"u_lev"),
                    k: loc(c"u_k"),
                    hdr: loc(c"u_hdr"),
                }
            });
            if linked.is_none() {
                plx_base::eventlog::log("video: the picture shader did not link; video will not be shown");
            }
            PROGRAM = Some(linked);
        }
        PROGRAM.as_ref().and_then(Option::as_ref)
    }
}

/// `(internal format, format)` for a texel of `bytes` bytes on this driver.
fn formats(bytes: c_int) -> (c_uint, c_uint) {
    match (gl_is_es(), bytes) {
        (_, 4) => (if gl_is_es() { GL_RGBA } else { GL_RGBA8 }, GL_RGBA),
        (true, 2) => (GL_LUMINANCE_ALPHA, GL_LUMINANCE_ALPHA),
        (true, _) => (GL_LUMINANCE, GL_LUMINANCE),
        (false, 2) => (GL_RG8, GL_RG),
        (false, _) => (GL_R8, GL_RED),
    }
}

impl Textures {
    /// Upload a picture's planes (two or three).
    ///
    /// # Safety
    /// Each plane's `data` must be readable for `stride * h` bytes, `stride >= w * bytes`.
    pub unsafe fn upload(&mut self, planes: &[Plane]) {
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
        let es = gl_is_es();
        let mut tight = Vec::new();
        for (i, p) in planes.iter().enumerate().take(3) {
            let shape = (p.w, p.h, p.bytes);
            if self.tex[i] == 0 {
                glGenTextures(1, &mut self.tex[i]);
            }
            glActiveTexture(GL_TEXTURE0 + i as c_uint);
            glBindTexture(GL_TEXTURE_2D, self.tex[i]);
            // A core profile reads rows `stride` apart itself; GLES2 has no row length, so a
            // padded plane is packed first. Every desktop is a core profile; this is the Mesa
            // GLES2 box's path, kept so that one renders the same picture.
            let row = p.w * p.bytes;
            let pixels = if p.stride == row {
                p.data
            } else if es {
                tight.clear();
                for y in 0..p.h as usize {
                    let src = std::slice::from_raw_parts(p.data.add(y * p.stride as usize), row as usize);
                    tight.extend_from_slice(src);
                }
                tight.as_ptr()
            } else {
                glPixelStorei(GL_UNPACK_ROW_LENGTH, p.stride / p.bytes);
                p.data
            };
            let (internal, format) = formats(p.bytes);
            if self.shape[i] != shape {
                glTexImage2D(GL_TEXTURE_2D, 0, internal as c_int, p.w, p.h, 0, format, GL_UNSIGNED_BYTE, pixels as *const c_void);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                tex_ledger::specified(self.tex[i], (p.w * p.bytes + 3) / 4, p.h);
                self.shape[i] = shape;
            } else {
                glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, p.w, p.h, format, GL_UNSIGNED_BYTE, pixels as *const c_void);
            }
            if !es && p.stride != row {
                glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
            }
        }
        glActiveTexture(GL_TEXTURE0);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    }

    /// Composite the uploaded picture under everything drawn so far this frame, into `rect` (canvas
    /// pixels, x/y/w/h), with black around it. Restores the app's blend.
    pub fn draw_under(&self, look: &Look, rect: [f32; 4]) {
        if self.tex[0] == 0 {
            return;
        }
        let Some(p) = program() else { return };
        use_prog(p.prog);
        unsafe {
            for (i, tex) in self.tex.iter().enumerate() {
                glActiveTexture(GL_TEXTURE0 + i as c_uint);
                glBindTexture(GL_TEXTURE_2D, *tex);
            }
            glActiveTexture(GL_TEXTURE0);
            glUniform4f(p.vid, rect[0] / SCR_W, rect[1] / SCR_H, rect[2] / SCR_W, rect[3] / SCR_H);
            glUniform1f(p.nv, if look.interleaved { 1.0 } else { 0.0 });
            glUniform1f(p.wide, if look.wide { 1.0 } else { 0.0 });
            glUniform1f(p.hi_a, if gl_is_es() { 1.0 } else { 0.0 });
            glUniform1f(p.max, look.max);
            glUniform4f(p.lev, look.levels[0], look.levels[1], look.levels[2], look.levels[3]);
            glUniform2f(p.k, look.k[0], look.k[1]);
            glUniform1f(p.hdr, match look.transfer {
                Transfer::Sdr => 0.0,
                Transfer::Pq => 1.0,
                Transfer::Hlg => 2.0,
            });
            glBlendFuncSeparate(GL_ONE_MINUS_DST_ALPHA, GL_ONE, GL_ONE_MINUS_DST_ALPHA, GL_ONE);
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
            glBlendFuncSeparate(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        }
    }

    /// Free the textures (the player stopped).
    pub fn release(&mut self) {
        for tex in &mut self.tex {
            super::delete_tex(*tex);
            *tex = 0;
        }
        self.shape = Default::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each normalized code lands where the standards put it.
    #[test]
    fn levels_map_black_white_and_neutral_chroma() {
        let y = |l: [f32; 4], code: f32, max: f32| (code / max - l[0]) * l[1];
        let c = |l: [f32; 4], code: f32, max: f32| (code / max - l[2]) * l[3];
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        // 8-bit limited: 16..235, chroma 16..240 about 128.
        let l = levels(false, 8, 255.0);
        assert!(close(y(l, 16.0, 255.0), 0.0) && close(y(l, 235.0, 255.0), 1.0));
        assert!(close(c(l, 128.0, 255.0), 0.0) && close(c(l, 240.0, 255.0), 0.5));
        // 10-bit limited, least-significant aligned (yuv420p10le): 64..940.
        let l = levels(false, 10, 1023.0);
        assert!(close(y(l, 64.0, 1023.0), 0.0) && close(y(l, 940.0, 1023.0), 1.0));
        assert!(close(c(l, 512.0, 1023.0), 0.0));
        // P010: the same codes shifted into the top of a 16-bit word.
        let l = levels(false, 16, 65535.0);
        assert!(close(y(l, 64.0 * 64.0, 65535.0), 0.0) && close(y(l, 940.0 * 64.0, 65535.0), 1.0));
        // 8-bit full range (JPEG): 0..255.
        let l = levels(true, 8, 255.0);
        assert!(close(y(l, 0.0, 255.0), 0.0) && close(y(l, 255.0, 255.0), 1.0));
        assert!(close(c(l, 128.0, 255.0), 0.0));
    }

    #[test]
    fn an_unspecified_matrix_follows_the_picture_size() {
        assert_eq!(matrix(2, 1080), [0.2126, 0.0722]);
        assert_eq!(matrix(2, 480), [0.299, 0.114]);
        assert_eq!(matrix(9, 2160), [0.2627, 0.0593]);
        assert_eq!(matrix(6, 1080), [0.299, 0.114]);
    }
}
