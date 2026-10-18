"""Build and install Misari for the current user, without restarting the desktop."""
import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "tools/wallpaperd/Cargo.toml"


def target_directory(manifest):
    result = subprocess.run(
        ["cargo", "metadata", "--offline", "--no-deps", "--format-version", "1"],
        cwd=manifest.parent, check=True, capture_output=True, text=True,
    )
    return Path(json.loads(result.stdout)["target_directory"])


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
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$").replace("%", "%%") + '"'


def main():
    parser = argparse.ArgumentParser(description="构建并安装 Misari、nirius 和壁纸客户端（含 Web 支持）")
    parser.add_argument("--profile", choices=("release", "debug"), default="release")
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--skip-build", action="store_true", help="安装已有构建产物")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    prefix = args.prefix.expanduser().resolve()
    manifests = [ROOT / "niri/Cargo.toml", ROOT / "nirius/Cargo.toml", MANIFEST]
    commands = []
    for manifest, binaries in zip(manifests, (["niri"], ["nirius"], ["wallpaperd", "misari-wallpaper"])):
        command = ["cargo", "build", "--locked"]
        if args.profile == "release":
            command.append("--release")
        if args.target_dir:
            command.extend(["--target-dir", str(args.target_dir.expanduser().resolve())])
        if manifest == MANIFEST:
            command.extend(["-p", "wallpaperd", "-p", "wallpaper-ui", "--features", "wallpaperd/web"])
        for binary in binaries:
            command.extend(["--bin", binary])
        commands.append((manifest.parent, command))
    if args.dry_run:
        for directory, command in commands:
            print(f"cd {shlex.quote(str(directory))} && {shlex.join(command)}")
        print(f"安装目录：{prefix}；跳过构建：{args.skip_build}")
        return
    if not args.skip_build:
        for directory, command in commands:
            subprocess.run(command, cwd=directory, check=True)
    targets = [args.target_dir.expanduser().resolve() if args.target_dir else target_directory(manifest) for manifest in manifests]
    target = targets[2]
    binaries = {
        "niri": targets[0] / args.profile / "niri",
        "nirius": targets[1] / args.profile / "nirius",
        **{name: target / args.profile / name for name in ("misari-wallpaper", "wallpaperd")},
    }
    for name, path in binaries.items():
        if not path.is_file():
            raise RuntimeError(f"尚未构建 {name}：{path}")
    library = steam_library(target, args.profile)
    app = prefix / "lib/misari-wallpaper"
    launcher = prefix / "bin/misari-wallpaper"
    cef = prefix / "lib/wallpaperd/web"
    release = target / args.profile
    runtime_files = [
        "libcef.so", "libvk_swiftshader.so", "libvulkan.so.1",
        "chrome_100_percent.pak", "chrome_200_percent.pak", "resources.pak",
        "icudtl.dat", "v8_context_snapshot.bin", "vk_swiftshader_icd.json",
        "chrome-sandbox", "LICENSE.txt", "CREDITS.html",
    ]
    for name in runtime_files + ["locales/en-US.pak"]:
        if not (release / name).is_file():
            raise RuntimeError(f"缺少 Web 运行库文件：{release / name}，请先构建启用 web 的 wallpaperd。")
    for name, source in binaries.items():
        destination = app / name if name == "misari-wallpaper" else prefix / "bin" / name
        atomic_copy(source, destination, 0o755)
    # The UI locates wallpaperd beside itself; keep only one installed binary.
    sibling = app / "wallpaperd"
    sibling.unlink(missing_ok=True)
    sibling.symlink_to("../../bin/wallpaperd")
    atomic_copy(library, app / "libsteam_api.so", 0o755)
    atomic_copy(ROOT / "tools/wallpaperd/crates/wallpaper-ui/assets/lucide/LICENSE", app / "LICENSE.lucide", 0o644)
    script = (
        "#!/bin/sh\n"
        f'exec {shlex.quote(str(app / "misari-wallpaper"))} "$@"\n'
    )
    atomic_write(launcher, script.encode(), 0o755)
    desktop = (ROOT / "packaging/misari-wallpaper.desktop").read_text()
    desktop = desktop.replace("Exec=misari-wallpaper", f"Exec={desktop_argument(str(launcher))}")
    atomic_write(prefix / "share/applications/io.misari.wallpaper.desktop", desktop.encode())
    atomic_copy(ROOT / "packaging/misari-wallpaper.svg", prefix / "share/icons/hicolor/scalable/apps/io.misari.wallpaper.svg", 0o644)
    for name in runtime_files:
        atomic_copy(release / name, cef / name, 0o755 if name == "chrome-sandbox" else 0o644)
    for source in (release / "locales").glob("*.pak"):
        atomic_copy(source, cef / "locales" / source.name, 0o644)

    resources = ROOT / "niri/resources"
    session = (resources / "niri-session").read_text().replace("exec niri --session", f"exec {shlex.quote(str(prefix / 'bin/niri'))} --session")
    atomic_write(prefix / "bin/niri-session", session.encode(), 0o755)
    session_desktop = (resources / "niri.desktop").read_text().replace("Name=Niri", "Name=Misari")
    session_desktop = session_desktop.replace("Exec=niri-session", f"Exec={desktop_argument(str(prefix / 'bin/niri-session'))}")
    atomic_write(prefix / "share/wayland-sessions/misari.desktop", session_desktop.encode())
    units = prefix / "share/systemd/user"
    for name, source, arguments in (
        ("niri", resources / "niri.service", "--session"),
        ("wallpaperd", ROOT / "tools/wallpaperd/configs/wallpaperd.service", "serve"),
    ):
        executable = str(prefix / "bin" / name).replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$")
        service = "\n".join(
            f'ExecStart="{executable}" {arguments}' if line.startswith("ExecStart=") else line
            for line in source.read_text().splitlines()
        ) + "\n"
        atomic_write(units / f"{name}.service", service.encode())
    atomic_copy(resources / "niri-shutdown.target", units / "niri-shutdown.target", 0o644)
    atomic_copy(resources / "niri-portals.conf", prefix / "share/xdg-desktop-portal/niri-portals.conf", 0o644)
    updater = shutil.which("update-desktop-database")
    if updater:
        subprocess.run([updater, str(prefix / "share/applications")], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if prefix == Path.home() / ".local" and shutil.which("systemctl"):
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=False)
    print(f"安装完成：{prefix}。当前桌面和服务保持运行。")
    print(f"登录器会话入口：{prefix / 'share/wayland-sessions/misari.desktop'}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError, StopIteration) as error:
        print(f"安装失败：{error}", file=sys.stderr)
        sys.exit(1)
