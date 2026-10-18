#!/usr/bin/env python3
"""Launch the official Wallpaper Engine UI through Proton and capture it with niri."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


STEAM = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "Steam"
ENGINE = Path.home() / "Share/Steam/steamapps/common/wallpaper_engine"
PROTON = STEAM / "steamapps/common/Proton 11.0"
RUNTIME = STEAM / "steamapps/common/SteamLinuxRuntime_sniper"
COMPAT = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / (
    "oh-my-niri/wallpaperd-compare/proton-ui-steam"
)
LOG = COMPAT / "ui-launch.log"


def steam_pids():
    return subprocess.run(
        ["pgrep", "-u", str(os.getuid()), "-x", "steam"],
        capture_output=True, text=True, check=False,
    ).stdout.split()


def desktop_environment():
    env = os.environ.copy()
    keys = {"DISPLAY", "WAYLAND_DISPLAY", "NIRI_SOCKET", "XDG_RUNTIME_DIR",
            "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS"}
    for pid in steam_pids():
        try:
            entries = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
        except OSError:
            continue
        for entry in entries:
            key, _, value = entry.decode(errors="surrogateescape").partition("=")
            if key in keys and value:
                env.setdefault(key, value)
    env.setdefault("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    if not env.get("NIRI_SOCKET"):
        sockets = list(Path(env["XDG_RUNTIME_DIR"]).glob("niri.*.sock"))
        if len(sockets) != 1:
            raise RuntimeError("无法确定 niri 会话，请设置 NIRI_SOCKET")
        env["NIRI_SOCKET"] = str(sockets[0])
    return env


def niri(env, *args):
    result = subprocess.run(
        ["niri", "msg", "--json", *map(str, args)], env=env,
        stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15,
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return json.loads(result.stdout) if result.stdout.strip() else None


def find_window(env):
    windows = niri(env, "windows", "--app-id", "^steam_app_431960$",
                   "--title", "^Wallpaper UI$")
    return max(windows, key=lambda window: window["id"], default=None)


def launch(env):
    if not steam_pids():
        raise RuntimeError("请先启动 Linux Steam 并登录账号")
    for path in (ENGINE / "wallpaper64.exe", PROTON / "proton", RUNTIME / "_v2-entry-point"):
        if not path.is_file():
            raise RuntimeError(f"找不到启动文件：{path}")
    drives = COMPAT / "pfx/dosdevices"
    drives.mkdir(parents=True, exist_ok=True)
    share = Path.home() / "Share"
    for letter in ("d:", "f:"):
        link = drives / letter
        if os.path.lexists(link):
            if not link.is_symlink() or link.resolve() != share.resolve():
                raise RuntimeError(f"盘符映射与壁纸目录不一致：{link}")
        else:
            link.symlink_to(share, target_is_directory=True)
    child_env = env.copy()
    for key in ("WINEPREFIX", "WINESERVER", "WINELOADER", "WINEDLLPATH"):
        child_env.pop(key, None)
    child_env.update(
        STEAM_COMPAT_CLIENT_INSTALL_PATH=str(STEAM),
        STEAM_COMPAT_DATA_PATH=str(COMPAT),
        STEAM_COMPAT_INSTALL_PATH=str(ENGINE),
        STEAM_COMPAT_APP_ID="431960",
        SteamAppId="431960", SteamGameId="431960", SteamOverlayGameId="431960",
        STEAM_COMPAT_TOOL_PATHS=f"{PROTON}:{RUNTIME}",
        STEAM_COMPAT_LIBRARY_PATHS=f"{STEAM}:{share / 'Steam'}",
        WINEDEBUG="-all", DXVK_LOG_LEVEL="error",
        WINEDLLOVERRIDES="winemenubuilder.exe=d;mscoree,mshtml=",
    )
    with LOG.open("w") as log:
        return subprocess.Popen(
            [str(RUNTIME / "_v2-entry-point"), "--verb=run", "--",
             str(PROTON / "proton"), "run", str(ENGINE / "wallpaper64.exe"),
             "-language", "schinese", "-showbrowse"],
            cwd=ENGINE, env=child_env, stdin=subprocess.DEVNULL,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )


def ensure_window(env):
    window = find_window(env)
    if window:
        return window, True
    process = launch(env)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        window = find_window(env)
        if window:
            niri(env, "action", "move-window-to-floating", "--id", window["id"])
            niri(env, "action", "set-window-width", "--id", window["id"], "1100")
            niri(env, "action", "set-window-height", "--id", window["id"], "760")
            time.sleep(2)
            return window, False
        if process.poll() not in (None, 0):
            raise RuntimeError(f"启动失败，退出码 {process.returncode}；日志：{LOG}")
        time.sleep(0.5)
    raise RuntimeError(f"等待 Wallpaper Engine 窗口超时；日志：{LOG}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("start", help="启动或聚焦已有界面，输出窗口 ID")
    commands.add_parser("status", help="查询窗口状态，输出 JSON")
    screenshot = commands.add_parser("screenshot", help="保存窗口截图，必要时自动启动")
    screenshot.add_argument("path", type=Path, help="PNG 输出路径")
    args = parser.parse_args()
    env = desktop_environment()
    if args.command == "status":
        window = find_window(env)
        result = {"running": window is not None,
                  "window_id": window["id"] if window else None}
    else:
        window, reused = ensure_window(env)
        result = {"window_id": window["id"], "reused": reused}
        if args.command == "start":
            niri(env, "action", "focus-window", "--id", window["id"])
            result["log"] = str(LOG)
        else:
            path = args.path.expanduser().resolve()
            path.parent.mkdir(parents=True, exist_ok=True)
            capture = niri(env, "action", "screenshot-window", "--id", window["id"],
                           "--path", path, "--silent", "--wait")
            result["path"] = capture["path"]
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        print(f"wallpaper-engine-ui: {error}", file=sys.stderr)
        sys.exit(1)
