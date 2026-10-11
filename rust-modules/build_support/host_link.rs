//! Link configuration for everything cargo itself LINKS on the developer's own machine, shared by
//! the build scripts of the application crate (`build.rs`) and the gfx layer crate
//! (`gfx/build.rs`): both produce a host executable (the simulator, a `--test` binary) that
//! reaches the `extern "C"` GL/SDL_ttf/nanosvg declarations in `plx_gfx`, and a `cargo:rustc-link-*`
//! line reaches only the package whose build script prints it (`rustc-link-lib` also flows to the
//! packages that depend on it; `rustc-link-arg`, which carries the nanosvg object everywhere but Windows, does not).
//!
//! A no-op for the build that ships: the television binary is linked by the Makefile, not by cargo.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Emit the host link configuration; `repo` is the repository root (where `src/svg.c` lives).
pub fn emit(repo: &Path) {
    // The television target is the one build cargo does not link, so it is the one that wants
    // none of this. It is identified by ARCHITECTURE rather than by the full triple: the target is
    // `arm-unknown-linux-gnueabi` (32-bit ARM, `target_arch = "arm"`), while every host that runs
    // this crate's tests is `aarch64` or `x86_64` — including CI's `ubuntu-24.04-arm` runner, whose
    // own arch is `aarch64` and which only ever asks for the ARM32 target explicitly.
    //
    // Read from `CARGO_CFG_TARGET_ARCH` rather than `cfg!`, because a build script is compiled for
    // the HOST: its own `cfg!` would answer for the script, not for the crate being built. Same
    // trap the `CARGO_FEATURE_*` env vars exist for.
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if target_arch == "arm" {
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if let Some(libdirs) = sdl_search_paths(&target_os) {
        for libdir in libdirs {
            println!("cargo:rustc-link-search=native={}", libdir.display());
        }
    } else {
        // Not fatal — a system-wide install may need no extra search path. This is not labelled
        // "hostsim": plain host tests now reach the same drawing symbols.
        println!(
            "cargo:warning=host link: could not discover SDL2/SDL2_ttf with the platform package \
             manager; relying on the default linker search path"
        );
    }

    println!("cargo:rustc-link-lib=dylib=SDL2");
    println!("cargo:rustc-link-lib=dylib=SDL2_ttf");

    match target_os.as_str() {
        "macos" => {
            // GL entry points come from the framework. The simulator asks for a 4.1 core context;
            // see `app.rs`'s context-attribute branch for why it cannot ask for GLES2 here.
            println!("cargo:rustc-link-lib=framework=OpenGL");
        }
        "windows" => {
            // The GL 1.1 export library every Windows toolchain ships; later entry points are
            // resolved through the context, which SDL does.
            println!("cargo:rustc-link-lib=dylib=opengl32");
        }
        _ => {
            println!("cargo:rustc-link-lib=dylib=GL");
        }
    }

    compile_svg(repo);
}

/// Build `src/svg.c` (the nanosvg rasterizer) for the host.
///
/// On the television the Makefile compiles this alongside `main.c` and `starfish.c` and links all
/// three with the staticlib. The simulator has no Makefile step, so it does the one piece it still
/// needs — `svg.c` is portable C with no webOS in it, and the icon set is rasterized from SVG at
/// runtime, so without it the UI links but has no icons.
///
/// Shelling out to the C compiler rather than taking the `cc` crate as a build-dependency: this is
/// one translation unit with two include paths, and the crate's dependency list is deliberately
/// short — a build-dep would be fetched for the ARM build too, which never runs this function.
fn compile_svg(repo: &Path) {
    let src = repo.join("src/svg.c");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("svg.o");
    println!("cargo:rerun-if-changed={}", src.display());

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let mut cmd = Command::new(&cc);
    cmd.arg("-c").arg(&src).arg("-o").arg(&out).arg("-O2");
    // PE/COFF code is position-independent by construction; MinGW's gcc warns on `-fPIC`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        cmd.arg("-fPIC");
    }
    let st = cmd
        // Matches the Makefile's include set for this file.
        .arg(format!("-I{}", repo.join("src").display()))
        .arg(format!("-I{}", repo.join("vendor/nanosvg").display()))
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "host link: could not run {cc:?} to compile {}: {e}",
                src.display()
            )
        });
    assert!(
        st.success(),
        "host link: compiling {} failed ({st})",
        src.display()
    );
    // On Windows the object goes in an archive, linked as a native library. A `rustc-link-arg`
    // lands at the END of the link line, after MinGW's C runtime (`-lmingwex -lmsvcrt`), and GNU
    // ld searches each archive once, in order: `svg.o`'s `fopen`, `qsort`, `sqrtf` and the rest
    // were left undefined (the first Windows CI link). A `rustc-link-lib` is placed with the
    // crate's own native libraries, ahead of the runtime. ELF and Mach-O links never hit this,
    // because `libc` is a shared library there and resolves from anywhere on the line.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let dir = out.parent().expect("OUT_DIR");
        let lib = dir.join("libplxsvg.a");
        let _ = std::fs::remove_file(&lib);
        let ar = std::env::var("AR").unwrap_or_else(|_| "ar".into());
        let st = Command::new(&ar)
            .arg("crs")
            .arg(&lib)
            .arg(&out)
            .status()
            .unwrap_or_else(|e| panic!("host link: could not run {ar:?} to archive svg.o: {e}"));
        assert!(st.success(), "host link: archiving svg.o failed ({st})");
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=plxsvg");
        return;
    }
    // Elsewhere, link the object directly; no intermediate archive, so no `ar` involved.
    //
    // The UNSUFFIXED form, which covers binaries, examples, benches and TESTS alike. It used to be
    // `-bins`, which reaches the simulator executable and nothing else, so the host test binary was
    // left without `svg.o`. That was invisible for as long as no test made `svg::rasterize`
    // reachable, and it stopped being invisible in restructure phase 5b, when `app/bridge.rs`'s
    // tests began mounting real screens that draw real icons: the hostsim test pass then failed on
    // `_svg_free`/`_svg_rasterize_rgba` ALONE, having already got SDL and GL from the
    // `rustc-link-lib` lines above, which do cover every target kind.
    //
    // Not `-tests` either, which looks like the precise answer and is not one: cargo rejects it
    // outright here ("does not have a test target"), because that suffix addresses declared
    // `[[test]]` integration targets and this crate has none — its tests are the LIB target
    // rebuilt with `--test`, which only the unsuffixed form reaches.
    println!("cargo:rustc-link-arg={}", out.display());
}

/// SDL library directories supplied by the host's package manager.
///
/// A successful pkg-config query may return no `-L` flags because the libraries live on the
/// linker's default path. That is still discovery success, represented by `Some(vec![])`, so a
/// normal Linux install does not emit a misleading Homebrew warning.
fn sdl_search_paths(target_os: &str) -> Option<Vec<PathBuf>> {
    if target_os == "linux" {
        if !Command::new("pkg-config")
            .args(["--exists", "sdl2", "SDL2_ttf"])
            .status()
            .ok()?
            .success()
        {
            return None;
        }
        let mut paths = Vec::new();
        for package in ["sdl2", "SDL2_ttf"] {
            // Query the raw variable rather than parsing shell-escaped `-L` flags: prefixes may
            // legitimately contain spaces.
            let out = Command::new("pkg-config")
                .args(["--variable=libdir", package])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
            if !path.as_os_str().is_empty() && !paths.contains(&path) {
                paths.push(path);
            }
        }
        return Some(paths);
    }

    if target_os == "windows" {
        // No package manager to ask that answers for the TARGET: the supported toolchain is
        // MSYS2's MinGW-w64, whose SDL2/SDL2_ttf packages install into the toolchain's own `lib`,
        // which its linker searches by default. A host pkg-config, when cross-compiling, would
        // answer with the host's directories instead. So: the default path, by design.
        return Some(Vec::new());
    }
    if target_os != "macos" {
        return None;
    }
    let out = Command::new("brew").arg("--prefix").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let prefix = String::from_utf8(out.stdout).ok()?;
    let libdir = Path::new(prefix.trim()).join("lib");
    libdir.is_dir().then(|| vec![libdir])
}
