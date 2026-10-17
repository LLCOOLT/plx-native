//! **The OpenGL entry points Windows does not export**, for the desktop build.
//!
//! Every GL call in the app is a plain `extern "C"` declaration resolved by the linker: the
//! television's `libGLESv2.so` and the Mac's OpenGL framework export the whole API, and so does
//! Linux's `libGL.so`. Windows' `opengl32.dll` exports OpenGL 1.1 and nothing later; everything
//! from 1.2 on (shaders, buffers, framebuffers, `glActiveTexture`) exists only as a pointer the
//! driver hands out for the current context. Without these definitions the Windows link fails on
//! `glUseProgram`, `glGetUniformLocation` and the rest.
//!
//! So each such entry point is DEFINED here, under its own name, as a function that asks
//! `SDL_GL_GetProcAddress` for the driver's pointer on its first call and jumps to it on every
//! call. The declarations elsewhere stay untouched and link to these. Every call happens on the
//! render thread after `SDL_GL_CreateContext`, and the app has one context, so one pointer per
//! name is right for the process. The 1.1 functions (`glClear`, `glBindTexture`, `glTexImage2D`,
//! `glGetString`...) are not here: `opengl32.dll` exports them, and a definition would shadow the
//! import.
//!
//! `extern "C"` is the right ABI: on x86-64 Windows `APIENTRY` (`__stdcall`) is the one x64
//! convention, the same as `"C"` and `"system"`.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

extern "C" {
    fn SDL_GL_GetProcAddress(name: *const c_char) -> *mut c_void;
}

/// The driver's pointer for `name`, cached in `slot`. A driver without it cannot draw this UI at
/// all (every one of these is core OpenGL 2.0 or 3.0), so the process stops with the name rather
/// than jumping through null.
fn entry(slot: &AtomicPtr<c_void>, name: &CStr) -> *mut c_void {
    let cached = slot.load(Ordering::Acquire);
    if !cached.is_null() {
        return cached;
    }
    // SAFETY: a NUL-terminated name; SDL answers null rather than failing.
    let found = unsafe { SDL_GL_GetProcAddress(name.as_ptr()) };
    if found.is_null() {
        let what = name.to_string_lossy();
        crate::eventlog::log(&format!(
            "gl: the driver has no {what}; OpenGL 3.0 is required"
        ));
        eprintln!("PlxNative: this PC's OpenGL driver has no {what} (OpenGL 3.0 is required)");
        std::process::abort();
    }
    slot.store(found, Ordering::Release);
    found
}

macro_rules! gl_entry_points {
    ($( fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?; )*) => {$(
        /// The driver's own entry point of this name.
        ///
        /// # Safety
        /// The GL prototype's: a current context, and every pointer valid for what the call reads
        /// or writes.
        #[no_mangle]
        pub unsafe extern "C" fn $name($($arg: $ty),*) $(-> $ret)? {
            static SLOT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
            const NAME: &CStr = match CStr::from_bytes_with_nul(concat!(stringify!($name), "\0").as_bytes()) {
                Ok(name) => name,
                Err(_) => panic!("a GL name with a NUL in it"),
            };
            // SAFETY: the pointer the driver gave for exactly this name, whose signature is the
            // one declared here (the Khronos prototype).
            let f: unsafe extern "C" fn($($ty),*) $(-> $ret)? = std::mem::transmute(entry(&SLOT, NAME));
            f($($arg),*)
        }
    )*};
}

gl_entry_points! {
    fn glActiveTexture(texture: c_uint);
    fn glAttachShader(program: c_uint, shader: c_uint);
    fn glBindAttribLocation(program: c_uint, index: c_uint, name: *const c_char);
    fn glBindBuffer(target: c_uint, buffer: c_uint);
    fn glBindFramebuffer(target: c_uint, framebuffer: c_uint);
    fn glBindVertexArray(array: c_uint);
    fn glBlendFuncSeparate(src_rgb: c_uint, dst_rgb: c_uint, src_a: c_uint, dst_a: c_uint);
    fn glBlitFramebuffer(
        sx0: c_int, sy0: c_int, sx1: c_int, sy1: c_int,
        dx0: c_int, dy0: c_int, dx1: c_int, dy1: c_int,
        mask: c_uint, filter: c_uint
    );
    fn glBufferData(target: c_uint, size: isize, data: *const c_void, usage: c_uint);
    fn glCheckFramebufferStatus(target: c_uint) -> c_uint;
    fn glCompileShader(shader: c_uint);
    fn glCreateProgram() -> c_uint;
    fn glCreateShader(ty: c_uint) -> c_uint;
    fn glDeleteFramebuffers(n: c_int, ids: *const c_uint);
    fn glEnableVertexAttribArray(index: c_uint);
    fn glFramebufferTexture2D(target: c_uint, attachment: c_uint, textarget: c_uint, texture: c_uint, level: c_int);
    fn glGenBuffers(n: c_int, buffers: *mut c_uint);
    fn glGenFramebuffers(n: c_int, ids: *mut c_uint);
    fn glGenVertexArrays(n: c_int, arrays: *mut c_uint);
    fn glGetProgramiv(program: c_uint, pname: c_uint, params: *mut c_int);
    fn glGetShaderInfoLog(shader: c_uint, bufsize: c_int, length: *mut c_int, infolog: *mut c_char);
    fn glGetShaderiv(shader: c_uint, pname: c_uint, params: *mut c_int);
    fn glGetUniformLocation(program: c_uint, name: *const c_char) -> c_int;
    fn glLinkProgram(program: c_uint);
    fn glShaderSource(shader: c_uint, count: c_int, string: *const *const c_char, length: *const c_int);
    fn glUniform1f(loc: c_int, x: f32);
    fn glUniform1i(loc: c_int, x: c_int);
    fn glUniform2f(loc: c_int, x: f32, y: f32);
    fn glUniform3f(loc: c_int, x: f32, y: f32, z: f32);
    fn glUniform4f(loc: c_int, x: f32, y: f32, z: f32, w: f32);
    fn glUniform4fv(loc: c_int, count: c_int, value: *const f32);
    fn glUseProgram(program: c_uint);
    fn glVertexAttribPointer(index: c_uint, size: c_int, ty: c_uint, normalized: u8, stride: c_int, ptr: *const c_void);
}
