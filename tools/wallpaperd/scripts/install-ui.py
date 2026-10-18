#!/usr/bin/env python3
"""Install the built native client into a user prefix, without restarting services."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[3]
MANIFEST = ROOT / "tools/wallpaperd/Cargo.toml"


def metadata():
    result = subprocess.run(
        ["cargo", "metadata", "--offline", "--no-deps", "--format-version", "1", "--manifest-path", str(MANIFEST)],
        check=True, capture_output=True, text=True,
    )
    return json.loads(result.stdout)


def steam_library(target, profile):
    # Prefer the library copied by this build. The registry fallback follows
    # Cargo.lock's exact package version and never assumes a registry hash.
    direct = target / profile / "libsteam_api.so"
    if direct.is_file():
        return direct
    import tomllib
    lock = tomllib.loads(MANIFEST.with_name("Cargo.lock").read_text())
    version = next(p["version"] for p in lock["package"] if p["name"] == "steamworks-sys")
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    candidates = sorted((cargo_home / "registry/src").glob(
        f"*/steamworks-sys-{version}/lib/steam/redistributable_bin/linux64/libsteam_api.so"
    ))
    if not candidates:
        raise RuntimeError("找不到此构建使用的 Steam SDK 运行库，请先构建 misari-wallpaper。")
    return candidates[0]


def atomic_write(destination, content, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(content)
        os.chmod(temporary, mode)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def atomic_copy(source, destination, mode):
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    os.close(fd)
    try:
        shutil.copyfile(source, temporary)
        os.chmod(temporary, mode)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def desktop_argument(value):
    # Desktop Exec uses its own quoting rules, not shell single quotes.
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$") + '"'


def main():
    parser = argparse.ArgumentParser(description="安装 Misari 原生壁纸客户端及桌面启动器")
    parser.add_argument("--profile", choices=("release", "debug"), default="release")
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    prefix = args.prefix.expanduser().resolve()
    target = args.target_dir.resolve() if args.target_dir else Path(metadata()["target_directory"])
    binaries = {name: target / args.profile / name for name in ("misari-wallpaper", "wallpaperd")}
    for name, path in binaries.items():
        if not path.is_file():
            raise RuntimeError(f"尚未构建 {name}：{path}")
    library = steam_library(target, args.profile)
    app = prefix / "lib/misari-wallpaper"
    launcher = prefix / "bin/misari-wallpaper"
    cef = Path.home() / ".local/lib/wallpaperd/web"
    plan = {
        "application": str(app / "misari-wallpaper"),
        "daemon": str(app / "wallpaperd"),
        "launcher": str(launcher),
        "desktop": str(prefix / "share/applications/io.misari.wallpaper.desktop"),
        "steam_library": str(library),
        "cef_runtime": str(cef),
        "cef_available": (cef / "libcef.so").is_file(),
        "profile": args.profile,
    }
    if args.dry_run:
        print(json.dumps(plan, ensure_ascii=False, indent=2))
        return
    for name, source in binaries.items():
        atomic_copy(source, app / name, 0o755)
    atomic_copy(library, app / "libsteam_api.so", 0o755)
    script = (
        "#!/bin/sh\n"
        f"MISARI_APP_DIR={shlex.quote(str(app))}\n"
        f"MISARI_CEF_DIR={shlex.quote(str(cef))}\n"
        'export LD_LIBRARY_PATH="$MISARI_APP_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}:$MISARI_CEF_DIR"\n'
        'exec "$MISARI_APP_DIR/misari-wallpaper" "$@"\n'
    )
    atomic_write(launcher, script.encode(), 0o755)
    desktop = (ROOT / "packaging/misari-wallpaper.desktop").read_text()
    desktop = desktop.replace("Exec=misari-wallpaper", f"Exec={desktop_argument(str(launcher))}")
    atomic_write(prefix / "share/applications/io.misari.wallpaper.desktop", desktop.encode())
    atomic_copy(ROOT / "packaging/misari-wallpaper.svg", prefix / "share/icons/hicolor/scalable/apps/io.misari.wallpaper.svg", 0o644)
    updater = shutil.which("update-desktop-database")
    if updater:
        subprocess.run([updater, str(prefix / "share/applications")], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    print(json.dumps({**plan, "installed": True, "message": "安装完成。可从应用菜单打开 Misari 壁纸。现有壁纸服务和桌面会话保持运行。"}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError, StopIteration) as error:
        print(f"安装失败：{error}", file=sys.stderr)
        sys.exit(1)
