#!/usr/bin/env python3
"""Unified wallpaperd checks: quick, compositor, pixel and Web suites."""
import argparse
from contextlib import nullcontext
import os
from pathlib import Path
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True
from support.headless import weston


ROOT = Path(__file__).resolve().parents[1]
IGNORED = ["--", "--ignored", "--nocapture", "--test-threads=1"]
AUDIO = ["pipewire", "pipewire-pulse", "wireplumber", "pactl", "paplay"]
# Each suite owns its Cargo selection and required programs. Weston means headless.
GROUPS = {
    "nested": (["--test", "nested"], ["weston", "dbus-run-session", "weston-simple-shm", *AUDIO]),
    "video": (["--test", "nested", "video_pixels_"], ["weston", "ffmpeg"]),
    "properties": (["--test", "nested", "rust_properties_"], ["weston"]),
    "mouse": (["--test", "nested", "mouse_"], ["weston", "weston-simple-shm"]),
    "library": (["--test", "nested", "library_import_"], ["weston"]),
    "audio": (["--test", "nested", "other_audio"], ["weston", *AUDIO]),
    "web-integration": (["--test", "nested", "web_project_"], ["weston"]),
    "web": (["--bin", "wallpaperd", "web_browser"], []),
    "web-audio": (["--test", "web_audio"], ["pipewire", "pipewire-pulse", "wireplumber", "pactl", "parec"]),
    "thumbnails": (["--test", "thumbnails"], ["ffmpeg"]),
    "multioutput": (["--test", "multioutput"], ["labwc", *AUDIO]),
    "pixels": (["--bin", "wallpaperd", "content::"], ["weston"]),
    "spectrum": (["--bin", "wallpaperd", "private_monitor_stereo_latest_pause_switch_and_reconnect"], AUDIO),
    "sounds": (["--bin", "wallpaperd", "scene_sound_gain_"], ["weston", *AUDIO]),
    "transitions": (["--bin", "wallpaperd", "gpu_geometric_transitions_"], ["weston"]),
    "sync": (["--bin", "wallpaperd", "shared_playback_"], ["weston"]),
}


def commands_for(group):
    if group == "quick":
        return [
            ["cargo", "fmt", "--package", "wallpaperd", "--package", "we-scene",
             "--package", "wallpaper-media", "--package", "we-web", "--check"],
            ["cargo", "clippy", "--workspace", "--exclude", "we-web", "--locked",
             "--all-targets", "--", "-D", "warnings"],
            ["cargo", "test", "--workspace", "--exclude", "we-web", "--locked"],
        ]
    cargo = ["cargo", "test", "--locked"]
    if group.startswith("web"):
        cargo += ["--features", "web"]
    command = [*cargo, *GROUPS[group][0]]
    command += IGNORED
    if group == "nested":
        command += ["--skip", "web_project_"]
    elif group == "pixels":
        command += ["--skip", "real_scene_pixels_pause_and_resume"]
    if group == "nested":
        command = ["dbus-run-session", "--", *command]
    commands = [command]
    if group == "web":
        commands.insert(0, ["cargo", "build", "--locked", "--features", "web"])
        commands += [
            ["cargo", "test", "--locked", "--package", "we-web", "--lib"],
            ["cargo", "test", "--locked", "--package", "we-web", "--test", "runtime",
             "--", "--ignored"],
        ]
    return commands


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("group", nargs="?", default="quick",
                        choices=["quick", *GROUPS])
    group = parser.parse_args().group
    targets, required = GROUPS.get(group, ([], []))
    programs = ["cargo", *required]
    missing = [program for program in programs if not shutil.which(program)]
    if missing:
        parser.error(f"{group} requires: {', '.join(missing)}")
    if targets[:2] == ["--test", "nested"]:
        value = os.environ.get("WALLPAPERD_TEST_NIRI", str(ROOT / "../../niri/target/release/niri"))
        niri = shutil.which(os.path.expanduser(value))
        if not niri:
            parser.error("build niri or set WALLPAPERD_TEST_NIRI; see tests/README.md")
        os.environ["WALLPAPERD_TEST_NIRI"] = str(Path(niri).resolve())
    environment = weston() if "weston" in required else nullcontext(dict(os.environ))
    print(f"wallpaperd checks: {group}", flush=True)
    with environment as env:
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        for command in commands_for(group):
            result = subprocess.run(command, cwd=ROOT, env=env)
            if result.returncode:
                return result.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
