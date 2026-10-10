#!/usr/bin/env python3
"""Build the Windows app: `PlxNative/` — the desktop build and every DLL it needs — and its zip.

The Windows counterpart of `ci/mkmacapp.py`, and the same three ways to ship something that works
only on the machine that built it:

  1. **Every DLL the executable imports, transitively, is copied beside it**, from the MinGW
     environment that linked it (MSYS2's `/mingw64/bin`). Windows searches the executable's own
     directory first, so a folder that carries them starts on a PC with none of them installed.
     `objdump -p` reads the import table; a DLL found in no searched directory is the system's
     (`KERNEL32.dll`, `opengl32.dll`, ...) and is left to Windows.
  2. **The dlopened libraries are not in any import table**, so the walk cannot see them: the
     bundled FFmpeg (`avutil-plx-61.dll` and the rest, built by `HOST=1 ci/build-ffmpeg.sh`) and
     libcurl (`libcurl-4.dll`, which `plx_net` binds at run time). They are named here, copied, and
     walked for their own imports in turn.
  3. **The app reads its payload from its own directory** (`paths::app_dir`, from `current_exe`):
     the fonts, the certificate roots and the licences go beside the executable, not in a
     subfolder.

Unsigned. Windows SmartScreen asks the person to confirm the first launch ("More info", then "Run
anyway") until the executable is signed with a code-signing certificate, which this project does
not have yet; `docs/windows-app.md` says so.

Usage:  ci/mkwinapp.py --exe PATH [--ffmpeg DIR] [--mingw DIR] [--out DIR] [--zip]
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Loaded at run time by name (`dynlib!` tables), so never in an import table.
FFMPEG_DLLS = ("avutil-plx-61.dll", "swresample-plx-7.dll", "avcodec-plx-63.dll", "avformat-plx-63.dll")
OPTIONAL_FFMPEG_DLLS = ("swscale-plx-10.dll",)
CURL_DLLS = ("libcurl-4.dll",)
PAYLOAD = ("appfont.ttf", "appfont-bold.ttf", "appfont-cjk.ttf", "OFL.txt", "le-roots.pem")


def imports(dll: Path) -> list[str]:
    out = subprocess.run(["objdump", "-p", str(dll)], check=True, text=True, capture_output=True).stdout
    return re.findall(r"DLL Name:\s*(\S+)", out)


def find(name: str, dirs: list[Path]) -> Path | None:
    for d in dirs:
        for cand in (d / name, d / name.lower()):
            if cand.is_file():
                return cand
    return None


def bundle(roots: list[Path], dirs: list[Path], dest: Path) -> list[str]:
    """Copy every non-system DLL `roots` import, transitively, into `dest`. Returns the copied names."""
    seen: set[str] = set()
    copied = []
    queue = list(roots)
    while queue:
        obj = queue.pop()
        for name in imports(obj):
            key = name.lower()
            if key in seen:
                continue
            seen.add(key)
            src = find(name, dirs)
            if src is None:
                continue  # the system's own
            target = dest / src.name
            if not target.exists():
                shutil.copy2(src, target)
                copied.append(src.name)
            queue.append(target)
    return copied


def version() -> str:
    """The version the binary reports: `rust-modules/build.rs`'s rule, as `ci/mkmacapp.py` spells it."""
    text = (REPO / "rust-modules/Cargo.toml").read_text()
    v = re.search(r'^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"', text, re.M)
    if not v:
        sys.exit("mkwinapp: no version in rust-modules/Cargo.toml")
    major, minor, patch = (int(x) for x in v.groups())
    if os.environ.get("PLX_RELEASE") == "1":
        return f"{major}.{minor}.{patch}"
    return f"{major}.{minor + 1}.0-dev"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True, help="the release plxnative-sim.exe")
    ap.add_argument("--ffmpeg", default=str(REPO / "vendor/ffmpeg-prefix-host/bin"))
    ap.add_argument("--mingw", default="/mingw64/bin")
    ap.add_argument("--out", default=str(REPO / "pkg"))
    ap.add_argument("--zip", action="store_true")
    args = ap.parse_args()

    exe = Path(args.exe)
    ffmpeg = Path(args.ffmpeg)
    mingw = Path(args.mingw)
    out = Path(args.out)
    app = out / "PlxNative"
    if app.exists():
        shutil.rmtree(app)
    app.mkdir(parents=True)

    print("==> executable")
    shutil.copy2(exe, app / "PlxNative.exe")

    print("==> dlopened libraries")
    roots = [app / "PlxNative.exe"]
    for name in FFMPEG_DLLS + OPTIONAL_FFMPEG_DLLS + CURL_DLLS:
        src = find(name, [ffmpeg, mingw])
        if src is None:
            if name in OPTIONAL_FFMPEG_DLLS:
                continue
            sys.exit(f"mkwinapp: {name} not found in {ffmpeg} or {mingw}")
        shutil.copy2(src, app / src.name)
        roots.append(app / src.name)
        print(f"    + {src.name}")

    print("==> imported libraries")
    for name in bundle(roots, [app, ffmpeg, mingw], app):
        print(f"    + {name}")

    print("==> payload")
    for f in PAYLOAD:
        shutil.copy2(REPO / "pkg" / f, app / f)
    shutil.copy2(REPO / "LICENSE", app / "LICENSE.txt")
    shutil.copy2(REPO / "LICENSING.md", app / "LICENSING.md")
    shutil.copy2(REPO / "THIRD-PARTY-NOTICES.md", app / "THIRD-PARTY-NOTICES.md")
    shutil.copytree(REPO / "licenses", app / "licenses")
    readme = REPO / "docs/windows-app.md"
    if readme.exists():
        shutil.copy2(readme, app / "README.md")

    print("==> verifying self-containment")
    missing = []
    for obj in sorted(app.glob("*.dll")) + [app / "PlxNative.exe"]:
        for name in imports(obj):
            if find(name, [app]) is None and find(name, [mingw]) is not None:
                missing.append(f"  {obj.name} -> {name}")
    if missing:
        sys.exit("mkwinapp: folder is NOT self-contained:\n" + "\n".join(missing))
    print(f"    {len(list(app.glob('*.dll')))} DLLs, nothing left in {mingw}")

    if args.zip:
        ver = version()
        z = out / f"PlxNative-{ver}-windows-x64.zip"
        print(f"==> {z.name}")
        with zipfile.ZipFile(z, "w", zipfile.ZIP_DEFLATED) as zf:
            for p in sorted(app.rglob("*")):
                zf.write(p, Path("PlxNative") / p.relative_to(app))


if __name__ == "__main__":
    main()
