#!/usr/bin/env python3
"""Run the selected comparison scheme on the left and current wallpaperd on the right."""

import argparse
import csv
import fcntl
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
import json
import os
import re
from pathlib import Path
import shutil
import signal
import stat
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

# Usually only change the project ID at the end, or pass an ID/path on the command line.
project = "~/Share/Steam/steamapps/workshop/content/431960/3479521040"
REPO = Path(__file__).resolve().parents[3]


def command(argv, env, timeout=3):
    result = subprocess.run([str(v) for v in argv], env=env, capture_output=True,
                            text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip() or str(argv))
    return result.stdout


def wait_for(probe, stopped, seconds=10):
    deadline = time.monotonic() + seconds
    while not stopped.is_set():
        value = probe()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise RuntimeError("等待会话就绪超时，请查看日志")
        stopped.wait(0.05)
    raise RuntimeError("启动已取消")


def group_projects(root, kind):
    paths = []
    for path in sorted(root.iterdir(), key=lambda p: (not p.name.isdecimal(),
                                                    int(p.name) if p.name.isdecimal() else p.name)):
        metadata = path / "project.json"
        if not metadata.is_file():
            continue
        try:
            actual = str(json.loads(metadata.read_text()).get("type", "")).strip().lower()
        except (OSError, ValueError, AttributeError) as error:
            print(f"跳过 {metadata}：{error}", file=sys.stderr)
            continue
        if actual == kind:
            paths.append(path)
    return paths


class Session:
    uses_wallpaperd_cli = True

    def __init__(self, name, binary, root, runtime, cache, parent, properties, stopped, bench=False,
                 backend=None, fps=30):
        self.name, self.binary = name, binary
        self.fps = fps
        self.bench = bench
        self.root, self.runtime = root / name, runtime / name
        self.root.mkdir()
        self.runtime.mkdir(mode=0o700)
        self.stopped = stopped
        self.children = []
        self.owned_processes = set()
        self.frame_log = self.root / "wallpaperd.log"
        self.cgroup = None
        self.env = parent.copy()
        display = self.env.get("WAYLAND_DISPLAY")
        if display and not Path(display).is_absolute() and parent.get("XDG_RUNTIME_DIR"):
            self.env["WAYLAND_DISPLAY"] = str(Path(parent["XDG_RUNTIME_DIR"]) / display)
        for key in ("NIRI_SOCKET", "WAYLAND_SOCKET", "DISPLAY", "WAYLAND_DEBUG",
                    "XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"):
            self.env.pop(key, None)
        self.env.update(XDG_RUNTIME_DIR=str(self.runtime),
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"),
                        XDG_CACHE_HOME=str(cache / name))
        config = self.root / "config/misari"
        state = self.root / "state/misari/wallpaperd"
        config.mkdir(parents=True)
        state.mkdir(parents=True)
        (config / "wallpaperd.toml").write_text(
            "pause_on_session = false\npause_on_fullscreen = false\n"
            "mute_on_other_audio = false\n[wallpaper_engine]\n"
            f'scene_backend = "{backend or name}"\n')
        (state / "state.json").write_text(json.dumps({"outputs": {"winit": {
            "properties": properties, "paused": False, "mute": True, "fps": fps}}}))
        (self.root / "niri.kdl").write_text(
            'animations { off; }\nhotkey-overlay { skip-at-startup; }\n'
            'xwayland-satellite { off; }\n'
            'output "winit" { scale 1; }\n')

    def spawn(self, argv, log, env=None):
        if self.stopped.is_set():
            raise RuntimeError("启动已取消")
        with (self.root / log).open("w") as stream:
            child = subprocess.Popen([str(v) for v in argv], env=self.env if env is None else env,
                                     stdin=subprocess.DEVNULL, stdout=stream,
                                     stderr=stream, start_new_session=True)
        self.children.append(child)
        if self.stopped.is_set():
            self.terminate()
            raise RuntimeError("启动已取消")
        return child

    def compositor(self, niri):
        self.niri = self.spawn([niri, "--config", self.root / "niri.kdl"], "niri.log")

        def sockets():
            if self.niri.poll() is not None:
                raise RuntimeError(f"{self.name}: 嵌套 niri 退出，查看 niri.log")
            paths = [p for p in self.runtime.iterdir() if stat.S_ISSOCK(p.stat().st_mode)]
            display = next((p for p in paths if p.name.startswith("wayland-")), None)
            ipc = next((p for p in paths if p.name.startswith("niri.")), None)
            return (display, ipc) if display and ipc else None

        display, ipc = wait_for(sockets, self.stopped)
        self.env.update(WAYLAND_DISPLAY=str(display), NIRI_SOCKET=str(ipc))

    def cli(self, *args, timeout=3):
        return json.loads(command([self.binary, *args], self.env, timeout))

    def play(self, path, paused=False):
        if paused:
            path_state = self.root / "state/misari/wallpaperd/state.json"
            saved = json.loads(path_state.read_text())
            saved["outputs"]["winit"]["paused"] = True
            path_state.write_text(json.dumps(saved))
        if not hasattr(self, "daemon"):
            if self.bench:
                self.env["WAYLAND_DEBUG"] = "client"
            self.daemon = self.spawn([self.binary, "serve"], "wallpaperd.log")
            self.env.pop("WAYLAND_DEBUG", None)

        def ready():
            if self.daemon.poll() is not None:
                raise RuntimeError(f"{self.name}: wallpaperd 退出，查看 wallpaperd.log")
            try:
                return self.cli("status")["state"]["outputs"].get("winit")
            except (RuntimeError, subprocess.TimeoutExpired):
                return None

        wait_for(ready, self.stopped)
        try:
            self.cli("set", str(path), "--output", "winit", "--fit", "cover", "--fps", str(self.fps),
                     "--mute", "true", "--transition", "cut", timeout=60)
        except (RuntimeError, subprocess.TimeoutExpired) as error:
            if self.stopped.is_set():
                raise
            self.state = ready() or {}
            self.state["error"] = str(error)
            print(f"{self.name}: 加载失败：{error}", flush=True)
            return

        def loaded():
            state = ready()
            return state if state and (state.get("backend") or state.get("error")) else None

        state = wait_for(loaded, self.stopped, seconds=60)
        self.state = state
        if state.get("error"):
            print(f"{self.name}: 加载失败：{state['error']}", flush=True)
        else:
            print(f"{self.name}: {state['backend']} 就绪", flush=True)

    def terminate(self):
        # Capture descendants before release can reparent surviving Chromium children.
        for child in reversed(self.children.copy()):
            self.owned_processes.update(resource_snapshot(child.pid)["cpu"])
        # Release while the daemon can still clean up its browsers and private files.
        if self.uses_wallpaperd_cli and self.cgroup is None and hasattr(self, "daemon") and self.daemon.poll() is None:
            try:
                self.cli("release", "--output", "winit", timeout=2)
            except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                pass
        for child in reversed(self.children.copy()):
            self.owned_processes.update(resource_snapshot(child.pid)["cpu"])
        self.signal_owned(signal.SIGTERM)

    def signal_owned(self, sig):
        for pid, started in tuple(self.owned_processes):
            try:
                fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
                # A reused PID must never signal another session's process group.
                if fields[19] == started:
                    os.killpg(int(fields[2]), sig)
            except (ProcessLookupError, FileNotFoundError):
                pass

    def close(self):
        self.terminate()
        deadline = time.monotonic() + 2
        for child in reversed(self.children):
            try:
                child.wait(timeout=max(0.01, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
            self.signal_owned(signal.SIGKILL)
            child.wait()
        self.children.clear()


def default_wine_config(kind):
    filename = "we-layerd-wine-system.toml" if kind == "web" else "we-layerd-wine.toml"
    return Path(__file__).resolve().parent / "configs" / filename


class WineSession(Session):
    """The historical we-layerd bridge, with its complete Wine workload in one cgroup."""

    def __init__(self, name, binary, root, runtime, cache, parent, properties, stopped, bench=False,
                 backend=None, fps=30, *, wine_config):
        self.service_env = parent.copy()
        super().__init__(name, binary, root, runtime, cache, parent, properties, stopped, bench, backend, fps)
        self.wine_config = tomllib.loads(wine_config.read_text())
        self.frame_log = self.root / "we-layerd.log"
        self.unit = f"wallpaperd-compare-wine-{self.runtime.parent.name}.service"
        self.env["WE_LAYERD_IPC_NAMESPACE"] = "." + self.runtime.parent.name
        self.env["DISPLAY"] = ""
        self.env["WINEDEBUG"] = "-all"
        self.prefix_lock = None

    def play(self, path, paused=False):
        if paused:
            raise RuntimeError("Wine 对照暂不支持加载时冻结时间")
        metadata = json.loads((path / "project.json").read_text())
        kind = str(metadata.get("type", "")).strip().lower()
        if kind not in ("scene", "web", "video"):
            raise RuntimeError(f"Wine 对照不支持项目类型：{kind}")
        cfg = self.wine_config
        wallpaper_file = path / metadata["file"] if kind == "video" else path / "project.json"
        prefix = Path(cfg["wine"]["env"]["WINEPREFIX"]).expanduser().resolve()
        if self.prefix_lock is None:
            prefix.parent.mkdir(parents=True, exist_ok=True)
            self.prefix_lock = prefix.with_suffix(".lock").open("w")
            try:
                fcntl.flock(self.prefix_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise RuntimeError(f"Wine 对照前缀正在使用：{prefix}") from None
        if not (prefix / "system.reg").is_file():
            raise RuntimeError(f"Wine 前缀尚未初始化：{prefix}；见 docs/compare.md")
        exe = Path(cfg["wine"]["wallpaper_exe"]).expanduser().resolve()
        app = self.root / "we-app"
        if not app.exists():
            app.mkdir()
            # WE writes config/logs beside its executable; keep those writes out of Steam.
            for entry in exe.parent.iterdir():
                if entry.name in ("config.json", "config_backups", "log.txt") or entry.suffix == ".mdmp":
                    continue
                if entry == exe:
                    shutil.copy2(entry, app / entry.name)
                else:
                    (app / entry.name).symlink_to(entry)
            source = json.loads((exe.parent / "config.json").read_text())
            private = {"?installdirectory": "Z:" + str(app).replace("/", "\\")}
            for user, settings in source.items():
                if not isinstance(settings, dict):
                    continue
                general = settings.get("general", {})
                user_settings = general.setdefault("user", {})
                user_settings.update(fps=self.fps, autostart=False, autostartscheduler=False,
                    playbackfullscreen="run", playbacksleep="run", playbackfocus="run",
                    playbackmaximized="run", playbackaudio="run", mediaintegration=False,
                    apprules=None, plugins=None)
                private[user] = {"general": general}
            # Proton reports "steamuser" rather than the host's Windows/Wine login.
            profile = next((value for value in private.values() if isinstance(value, dict)), None)
            if profile is not None:
                private.setdefault("steamuser", profile)
            (app / "config.json").write_text(json.dumps(private, ensure_ascii=False))
        output = json.loads(command([self.niri_binary, "msg", "--json", "outputs"], self.env))["winit"]
        size = output["modes"][output["current_mode"]]
        width, height = size["width"], size["height"]
        cfg["general"].update(backend="layer_shell", fps_limit=self.fps,
                              restart_wine_on_exit=False, hide_debug_window=False)
        cfg["isolation"].update(mode="gamescope_headless", width=width, height=height)
        cfg["wine"].update(wallpaper_exe=str(app / exe.name), args=["-control", "openWallpaper",
            "-file", str(wallpaper_file), "-playInWindow", "WE-DEBUG-WINDOW",
            "-width", str(width), "-height", str(height), "-x", "0", "-y", "0"])
        cfg["wine"]["env"]["WINEPREFIX"] = str(prefix)
        cfg["runtime"] = {"mode": "wine_layerd", "wallpaper_type": kind}
        config = self.root / "we-layerd.toml"
        tables = []
        for section, values in cfg.items():
            tables.append(f"[{section}]")
            tables.extend(f"{key} = {json.dumps(value, ensure_ascii=False)}"
                          for key, value in values.items() if not isinstance(value, dict))
            for key, value in values.items():
                if isinstance(value, dict):
                    tables.append(f"[{section}.{key}]")
                    tables.extend(f"{k} = {json.dumps(v, ensure_ascii=False)}" for k, v in value.items())
        config.write_text("\n".join(tables) + "\n")
        if not hasattr(self, "daemon"):
            if self.bench:
                self.env["WAYLAND_DEBUG"] = "client"
            forwarded = ("WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME",
                "XDG_CACHE_HOME", "NIRI_SOCKET", "PULSE_SERVER", "DBUS_SESSION_BUS_ADDRESS",
                "DISPLAY", "DRI_PRIME", "MESA_VK_DEVICE_SELECT", "MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE",
                "DISABLE_LSFG", "WGPU_BACKEND", "WAYLAND_DEBUG", "WINEDEBUG",
                "WE_LAYERD_IPC_NAMESPACE")
            self.daemon = self.spawn(["systemd-run", "--user", "--wait", "--pipe", "--collect",
                "--expand-environment=no", f"--unit={self.unit}", "--property=Type=exec",
                "--property=KillMode=control-group", "--property=TimeoutStopSec=5",
                *[f"--setenv={k}={self.env[k]}" for k in forwarded if k in self.env],
                self.binary, "run", "--config", config], self.frame_log.name, env=self.service_env)
            self.env.pop("WAYLAND_DEBUG", None)

            def scope():
                if self.daemon.poll() is not None:
                    raise RuntimeError("Wine 单元启动失败，查看 we-layerd.log")
                value = command(["systemctl", "--user", "show", self.unit, "-p", "ControlGroup",
                                 "--value"], self.service_env).strip()
                return Path("/sys/fs/cgroup") / value.lstrip("/") if value else None

            self.cgroup = wait_for(scope, self.stopped)
        else:
            command([self.binary, "switch", "--config", config], self.env, timeout=60)

        def ready():
            if self.daemon.poll() is not None:
                raise RuntimeError("Wine 对照退出，查看 we-layerd.log")
            try:
                status = tomllib.loads(command([self.binary, "ctl", "status"], self.env))
            except (RuntimeError, subprocess.TimeoutExpired):
                return None
            log = self.frame_log.read_text(errors="replace")
            if (status.get("orchestrator", {}).get("phase") == "running"
                    and str(wallpaper_file) in status.get("wine", {}).get("args", [])
                    and "using X11 window" in log and "wayland multi-output loop started" in log):
                return status
            return None

        status = wait_for(ready, self.stopped, seconds=60)
        wine_env = self.env | cfg["wine"]["env"] | status["wine"]["env"]
        command([cfg["wine"]["command"], cfg["wine"]["wallpaper_exe"], "-control", "mute"],
                wine_env, timeout=20)
        self.state = {"backend": "official_we_wine", "error": None, "status": status,
                      "source_exe": str(exe), "wineprefix": str(prefix), "cgroup": str(self.cgroup)}
        print(f"{self.name}: 官方 WE Wine 就绪 ({width}x{height})", flush=True)

    def compositor(self, niri):
        self.niri_binary = niri
        return super().compositor(niri)

    def terminate(self):
        if hasattr(self, "daemon"):
            subprocess.run(["systemctl", "--user", "stop", "--no-block", self.unit],
                           env=self.service_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        super().terminate()

    def close(self):
        try:
            if hasattr(self, "daemon"):
                subprocess.run(["systemctl", "--user", "stop", self.unit], timeout=10,
                               env=self.service_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        finally:
            super().close()
            if self.prefix_lock is not None:
                self.prefix_lock.close()
                self.prefix_lock = None


def start_layerd(session, project, fps, shm=False):
    config = session.root / "we-layerd.toml"
    assets = Path.home() / "Share/Steam/steamapps/common/wallpaper_engine/assets"
    config.write_text('[general]\nscale_mode = "cover"\n[renderer]\n' +
        f'source = {json.dumps(str(project))}\nassets_path = {json.dumps(str(assets))}\n' +
        f'fps = {fps}\nmax_fps = {fps}\nmuted = true\nvolume = 0.0\nmsaa_samples = 1\n'
        f'prefer_dmabuf = {str(not shm).lower()}\n'
        '[integrations]\nmedia = false\naudio_spectrum = false\n'
        '[rules]\nfocused = "keep"\nmaximized = "keep"\nfullscreen = "keep"\n')
    if hasattr(session, "daemon"):
        command([session.binary, "--cli", "switch", "--config", config], session.env, timeout=60)
        return session.daemon
    return session.spawn([session.binary, "--cli", "run", "--config", config], "app.log")


class LayerdSession(Session):
    uses_wallpaperd_cli = False

    def play(self, path, paused=False):
        kind = json.loads((path / "project.json").read_text())["type"].lower()
        self.frame_log = self.root / "app.log"
        if self.bench:
            self.env["WAYLAND_DEBUG"] = "client"
        self.daemon = start_layerd(self, path, self.fps, shm=kind == "video")

        def ready():
            if self.daemon.poll() is not None:
                raise RuntimeError("we-layerd 退出，查看 app.log")
            try:
                state = tomllib.loads(command([self.binary, "--cli", "ctl", "status"], self.env))
                return state if state.get("orchestrator", {}).get("phase") == "running" else None
            except (RuntimeError, subprocess.TimeoutExpired):
                return None

        self.state = dict(wait_for(ready, self.stopped, seconds=60), backend="we-layerd")
        if paused:
            command([self.binary, "--cli", "ctl", "pause"], self.env)
        print("we-layerd: renderer 就绪", flush=True)


METRICS = (("actual_fps", "FPS"), ("cpu_pct", "CPU %"),
           ("pss_mib", "PSS MiB"), ("vram_mib", "VRAM MiB"),
           ("gpu_3d_pct", "GPU %"))


class FrameCounter:
    """Count buffer-bearing surface commits, not callback requests or FPS limits."""
    request = re.compile(r"-> wl_surface[@#](\d+)\.(attach|commit)\((.*)\)")
    timestamp = re.compile(r"\[\s*(?:(\d+):(\d+):)?(\d+\.\d+)\]")

    def __init__(self, path):
        self.stream = path.open()
        self.pending = {}
        self.frames = 0
        self.observed = False
        self.last_commit = {}
        self.intervals = []

    def read(self):
        while True:
            offset = self.stream.tell()
            line = self.stream.readline()
            if not line.endswith("\n"):
                self.stream.seek(offset)
                break
            match = self.request.search(line)
            if not match:
                continue
            surface, method, arguments = match.groups()
            if method == "attach":
                self.pending[surface] = "wl_buffer" in arguments
            elif self.pending.pop(surface, False):
                self.frames += 1
                self.observed = True
                stamp = self.timestamp.search(line)
                if stamp:
                    milliseconds = float(stamp[3])
                    if stamp[1] is not None:
                        milliseconds = ((int(stamp[1]) * 60 + int(stamp[2])) * 60 + milliseconds) * 1000
                    previous = self.last_commit.get(surface)
                    if previous is not None and milliseconds >= previous:
                        self.intervals.append(milliseconds - previous)
                    self.last_commit[surface] = milliseconds
        return self.frames if self.observed else None


def drm_fields(text):
    return dict(line.split(":", 1) for line in text.splitlines() if ":" in line)


def memory_mib(value):
    parts = value.split()
    units = {"B": 1, "KiB": 1024, "kB": 1024, "MiB": 1024 ** 2}
    return int(parts[0]) * units[parts[1] if len(parts) > 1 else "B"] / 1024 ** 2


def resource_snapshot(group, proc=Path("/proc"), cgroup=None):
    cpu, pss, clients = {}, 0.0, {}
    pss_available = True
    processes = {}
    for directory in proc.iterdir():
        if not directory.name.isdecimal():
            continue
        try:
            fields = (directory / "stat").read_text().rsplit(")", 1)[1].split()
            if len(fields) >= 20 and fields[0] != "Z":
                processes[int(directory.name)] = (directory, fields)
        except (OSError, IndexError, ValueError):
            continue
    if cgroup is not None:
        members = {int(pid) for path in cgroup.rglob("cgroup.procs") for pid in path.read_text().split()}
    else:
        members = {group} | {pid for pid, (_, fields) in processes.items() if int(fields[2]) == group}
        # Workers and Chromium may own separate process groups; include their descendants.
        while True:
            found = {pid for pid, (_, fields) in processes.items() if int(fields[1]) in members}
            if found <= members:
                break
            members |= found
    for pid, (directory, fields) in processes.items():
        if pid not in members:
            continue
        cpu[(pid, fields[19])] = int(fields[11]) + int(fields[12])
        try:
            info = drm_fields((directory / "smaps_rollup").read_text())
            pss += memory_mib(info["Pss"])
        except (OSError, KeyError, ValueError):
            pss_available = False
        try:
            descriptors = list((directory / "fdinfo").iterdir())
        except OSError:
            descriptors = []
        for descriptor in descriptors:
            try:
                info = {key: value.strip() for key, value in drm_fields(descriptor.read_text()).items()}
                if "drm-client-id" not in info:
                    continue
                identity = (info.get("drm-pdev", info.get("drm-driver", "")), info["drm-client-id"])
                clients.setdefault(identity, info)
            except OSError:
                continue
    engines, vram = {}, []
    for identity, info in clients.items():
        value = info.get("drm-resident-vram", info.get("drm-memory-vram"))
        if value is not None:
            vram.append(memory_mib(value))
        for key, value in info.items():
            if not key.startswith("drm-engine-") or key.startswith("drm-engine-capacity-"):
                continue
            name = key.removeprefix("drm-engine-")
            if name not in ("gfx", "render", "render0", "3d"):
                continue
            capacity = max(1, int(info.get(f"drm-engine-capacity-{name}", "1")))
            engines[(*identity, name)] = (int(value.split()[0]), capacity)
    return {"time": time.monotonic(), "cpu": cpu, "engines": engines,
            "pss_mib": pss if cpu and pss_available else None,
            "vram_mib": sum(vram) if vram else None}


def resource_delta(before, after):
    seconds = after["time"] - before["time"]
    cpu = sum(max(0, ticks - before["cpu"].get(pid, ticks))
              for pid, ticks in after["cpu"].items())
    row = {"cpu_pct": cpu / os.sysconf("SC_CLK_TCK") / seconds * 100 if after["cpu"] else None,
           "pss_mib": after["pss_mib"], "vram_mib": after["vram_mib"],
           "gpu_3d_pct": None}
    for identity, (ticks, capacity) in list(after["engines"].items()):
        previous = before["engines"].get(identity)
        if previous is None:
            continue
        # DRM counters may briefly regress; retain the high-water mark.
        after["engines"][identity] = (max(ticks, previous[0]), capacity)
        usage = max(0, ticks - previous[0]) / 1e9 / seconds / capacity * 100
        row["gpu_3d_pct"] = (row["gpu_3d_pct"] or 0.0) + usage
    old_frames, frames = before.get("frames"), after.get("frames")
    row["actual_fps"] = (frames - old_frames) / seconds if frames is not None and old_frames is not None else None
    return row


def print_benchmark(rows, path, width, height, seconds, names=None, fps=30,
                    kind=None, project_names=None):
    labels = {"wine": "Wine", "linux-wallpaperengine": "LWE", "waywallen": "waywallen",
              "we-layerd": "we-layerd", "wallpaperd": "wallpaperd"}
    # Historical native/rust references keep their actual identities.
    names = list(labels) if names is None or all(name in labels for name in names) else names
    headers = ["Type", "Metrics", *[f"{index} {labels.get(name, name)}"
                                    for index, name in enumerate(names, 1)]]
    groups = []
    for wallpaper_type in ("scene", "video", "web"):
        group = []
        for index, (key, label) in enumerate(METRICS):
            cells = []
            for backend in names:
                samples = [row for row in rows if row["backend"] == backend
                           and row.get("type", kind) == wallpaper_type]
                if not samples or any(row[key] is None for row in samples):
                    cells.append("N/A")
                else:
                    mean = sum(row[key] * row["interval_s"] for row in samples) / sum(
                        row["interval_s"] for row in samples)
                    cells.append(f"{mean:.1f}")
            group.append([wallpaper_type if index == 0 else "",
                          label.removesuffix(" MiB").replace(" ", ""), *cells])
        groups.append(group)
    widths = [max(len(row[index]) for row in [headers, *[r for g in groups for r in g]])
              for index in range(len(headers))]
    border = "+" + "+".join("-" * (size + 2) for size in widths) + "+"
    print(f"\nbench: {seconds:g}s | sample: 1s | {width}x{height} | target: {fps} fps")
    print(border)
    print("|" + "|".join(f" {value:<{size}} " for value, size in zip(headers, widths)) + "|")
    print(border)
    for group in groups:
        for row in group:
            print("|" + "|".join(f" {value:<{size}} " if index < 2 else f" {value:>{size}} "
                  for index, (value, size) in enumerate(zip(row, widths))) + "|")
        print(border)
    for wallpaper_type in ("scene", "video", "web"):
        print(f"{wallpaper_type} name: {(project_names or {}).get(wallpaper_type, 'N/A')}")
    print("\nFPS: avg (Wayland buffer commits/s).\nCPU%: avg; one core = 100%; may exceed 100%.\n"
          "PSS / VRAM: avg, MiB.\nGPU%: avg, process 3D engine usage.\n"
          f"All averages are weighted by sample interval; N/A = unmeasured or unavailable.\ncsv: {path}", flush=True)


def benchmark(sessions, root, stopped, width, height, duration=30, warmup=0, project=None):
    rows, counters, previous = [], [], []
    path = root / "bench.csv"
    try:
        if stopped.wait(warmup):
            return 0
        for session in sessions:
            counter = FrameCounter(session.frame_log)
            counters.append(counter)
            sample = resource_snapshot(session.daemon.pid, cgroup=session.cgroup)
            sample["frames"] = counter.read()
            counter.intervals.clear()
            previous.append(sample)
        start = time.monotonic()
        print(f"采样 {duration}s…", flush=True)
        with path.open("w", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=["elapsed_s", "interval_s", "backend", *[key for key, _ in METRICS]])
            writer.writeheader()
            stream.flush()
            for index in range(1, duration + 1):
                if stopped.wait(max(0, start + index - time.monotonic())):
                    break
                for side, (session, counter) in enumerate(zip(sessions, counters)):
                    if stopped.is_set():
                        break
                    if session.niri.poll() is not None or session.daemon.poll() is not None:
                        if stopped.is_set():
                            break
                        raise RuntimeError(f"{session.name}: bench 期间进程退出；已保留采样 CSV")
                    sample = resource_snapshot(session.daemon.pid, cgroup=session.cgroup)
                    sample["frames"] = counter.read()
                    if stopped.is_set():
                        break
                    row = resource_delta(previous[side], sample)
                    row.update(backend=session.name, elapsed_s=sample["time"] - start,
                               interval_s=sample["time"] - previous[side]["time"])
                    previous[side] = sample
                    rows.append(row)
                    writer.writerow({key: f"{value:.6f}" if isinstance(value, float) else value
                                     for key, value in row.items()})
                stream.flush()
        seconds = min((row["elapsed_s"] for row in rows[-2:]), default=0)
        metadata = json.loads((project / "project.json").read_text()) if project else {}
        kind = str(metadata.get("type", "")).strip().lower()
        print_benchmark(rows, path, width, height, duration if not stopped.is_set() else round(seconds, 1),
                        [session.name for session in sessions], fps=sessions[0].fps, kind=kind,
                        project_names={kind: metadata.get("title") or (project.name if project else "N/A")})
        pacing = {}
        for session, counter in zip(sessions, counters):
            intervals = sorted(counter.intervals)
            if intervals:
                pacing[session.name] = {"frames": len(intervals), "mean_ms": statistics.mean(intervals),
                    "p50_ms": statistics.median(intervals),
                    "p95_ms": intervals[min(len(intervals) - 1, int(len(intervals) * .95))],
                    "max_ms": intervals[-1]}
                value = pacing[session.name]
                print(f"{session.name} commit interval: mean {value['mean_ms']:.3f} ms; p95 {value['p95_ms']:.3f} ms")
        (root / "frame-times.json").write_text(json.dumps({"source": "Wayland buffer commit intervals (not CPU/GPU render duration)",
            "sides": pacing}, indent=2))
        return 0
    finally:
        for counter in counters:
            counter.stream.close()


COMPARATORS = {
    "native": (Session, REPO / "output/wallpaperd-before-rust-native-20261005-030945/release-wallpaperd", "native"),
    "wine": (WineSession, REPO / "3rd/wallpaper/desktop/we-layerd-wine/target/release/we-layerd", "wine"),
    "rust": (Session, None, "rust"),
    "we-layerd": (LayerdSession, REPO / "3rd/wallpaper/runtime/we-layerd-native/squashfs-root/AppRun", "we-layerd"),
}


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", nargs="?", default=project, help="Workshop ID 或项目目录；轮播时可指定 Workshop 根目录")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--bench", action="store_true", help="加载后采样 30 秒，打印资源表并保存 bench.csv")
    mode.add_argument("--loop", choices=("video", "scene", "we_scene", "web"), help="按类型循环对比；we_scene 是 scene 的别名")
    parser.add_argument("--duration", type=int, help="每项轮播秒数（默认 5）或 bench 采样秒数（默认 30）")
    parser.add_argument("--fps", type=int, default=30, help="两边 FPS 上限（默认 30）")
    parser.add_argument("--warmup", type=float, default=3, help="bench 暖机秒数（默认 3）")
    parser.add_argument("--compare", dest="comparator", choices=COMPARATORS,
                        help="左侧对照方案（we-layerd 为 0.2.9；native 为旧 wallpaperd）；右侧为最新版 wallpaperd")
    parser.add_argument("--reference", type=Path, help="覆盖左侧方案的二进制")
    parser.add_argument("--wallpaperd", type=Path, help="指定右侧二进制；省略时构建最新版 dev，--bench 使用 release")
    parser.add_argument("--wine-config", type=Path,
                        help="覆盖历史 we-layerd 配置；默认 scene/video 用 GE Wine，web 用系统 Wine")
    parser.add_argument("--niri", type=Path, default=Path.home() / ".local/lib/niri-shm/niri")
    args = parser.parse_args(argv)
    args.comparator_explicit = args.comparator is not None
    args.reference_explicit = args.reference is not None
    args.comparator = args.comparator or "native"
    args.reference = args.reference or COMPARATORS[args.comparator][1]
    if args.reference is None:
        parser.error(f"--compare {args.comparator} 需要 --reference 指定对照二进制")
    args.build_wallpaperd = args.wallpaperd is None
    args.wallpaperd = args.wallpaperd or REPO / "tools/wallpaperd/target" / ("release" if args.bench else "debug") / "wallpaperd"
    if COMPARATORS[args.comparator][0] is WineSession and args.wine_config is not None:
        if not args.wine_config.is_file():
            parser.error(f"Wine 配置不存在：{args.wine_config}")
    if args.duration is None:
        args.duration = 5 if args.loop else 30
    if args.duration < 1:
        parser.error("--duration must be positive")
    if not 1 <= args.fps <= 240 or args.warmup < 0 or not args.warmup < float("inf"):
        parser.error("--fps must be in 1..240 and --warmup must be finite and nonnegative")
    return parser, args


def main():
    parser, args = parse_args()
    path = (Path(project).parent / args.project if args.project.isdecimal() else Path(args.project)).expanduser().resolve()
    if args.loop:
        source = path.parent if (path / "project.json").is_file() or args.project == project else path
        if not source.is_dir():
            parser.error(f"Workshop 根目录不存在: {source}")
        kind = "scene" if args.loop == "we_scene" else args.loop
        paths = group_projects(source, kind)
        if not paths:
            parser.error(f"目录中没有 {args.loop} 项目: {source}")
        path = paths[0]
    else:
        if not (path / "project.json").is_file():
            parser.error(f"项目缺少 project.json: {path}")
        kind = str(json.loads((path / "project.json").read_text()).get("type", "")).strip().lower()
        if kind not in ("scene", "video", "web"):
            parser.error(f"仅支持 scene/video/web，本项目为 {kind}")
    if kind == "web":
        if not args.comparator_explicit:
            args.comparator = "wine"
            if not args.reference_explicit:
                args.reference = COMPARATORS["wine"][1]
        if args.comparator == "native":
            parser.error("native 基准不支持 Web；使用 --compare wine 或 --compare rust --reference /path/to/web-wallpaperd")
        if args.build_wallpaperd:
            args.wallpaperd = REPO / "tools/wallpaperd/target/we-web" / ("release" if args.bench else "debug") / "wallpaperd"
    factory, _, reference_backend = COMPARATORS[args.comparator]
    wine_mode = factory is WineSession
    if wine_mode:
        args.wine_config = args.wine_config or default_wine_config(kind)
        if not args.wine_config.is_file():
            parser.error(f"Wine 配置不存在：{args.wine_config}")
    for binary in (args.reference, args.niri, *([] if args.build_wallpaperd else [args.wallpaperd])):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"对照方案或 niri 二进制不存在或不可执行: {binary}")
    parent = os.environ.copy()
    parent_runtime = Path(parent.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
    display = parent_runtime / parent.get("WAYLAND_DISPLAY", "")
    if not display.exists() or not stat.S_ISSOCK(display.stat().st_mode):
        parser.error("请在现有 niri Wayland 会话的终端中运行")
    parent["WAYLAND_DISPLAY"] = str(display)
    parent.setdefault("PULSE_SERVER", f"unix:{parent_runtime}/pulse/native")
    parent.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path={parent_runtime}/bus")
    # Terminals may retain a stale NIRI_SOCKET after the main compositor restarts.
    focused = None
    socket_prefix = f"niri.{display.name}."
    for ipc in dict.fromkeys([parent.get("NIRI_SOCKET", ""), *map(str, parent_runtime.glob(socket_prefix + "*.sock"))]):
        if not ipc or not Path(ipc).name.startswith(socket_prefix):
            continue
        parent["NIRI_SOCKET"] = ipc
        try:
            focused = json.loads(command([args.niri, "msg", "--json", "focused-output"], parent))
            if focused:
                break
        except (RuntimeError, subprocess.TimeoutExpired):
            pass
    if not focused:
        parser.error("找不到可用的父 niri IPC socket")
    if args.build_wallpaperd:
        print(f"构建右侧最新版 wallpaperd（{'release' if args.bench else 'dev'}）…", flush=True)
        build = ["cargo", "build", "--locked", "-p", "wallpaperd"]
        if args.bench:
            build.append("--release")
        if kind == "web":
            build.extend(["--features", "web", "--target-dir", "target/we-web"])
        result = subprocess.run(build, cwd=REPO / "tools/wallpaperd")
        if result.returncode:
            raise RuntimeError("最新版 wallpaperd 构建失败")
    if not args.wallpaperd.is_file() or not os.access(args.wallpaperd, os.X_OK):
        parser.error(f"wallpaperd 不存在或不可执行：{args.wallpaperd}")
    properties = {}
    saved = Path(parent.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "misari/wallpaperd/state.json"
    if saved.exists() and not args.loop and not wine_mode and factory is not LayerdSession:
        outputs = json.loads(saved.read_text()).get("outputs", {})
        properties = outputs.get(focused["name"], next(iter(outputs.values()), {})).get("properties", {})
    root = REPO / "output/compare" / (f"loop-{args.loop}" if args.loop else path.name) / datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    root.mkdir(parents=True, mode=0o700)
    # Short Unix socket paths are required; logs and state remain in output/.
    runtime = Path(tempfile.mkdtemp(prefix="wd-compare-", dir=parent_runtime))
    cache = Path(parent.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "misari/wallpaperd-compare"
    stopped = threading.Event()
    sessions = []

    def stop(*_):
        stopped.set()
        # Also unblock a CLI waiting for a long scene load before joining its thread.
        for session in sessions:
            session.terminate()

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    started = time.monotonic()
    ending = "采样完成后自动退出；Ctrl+C 可提前结束。" if args.bench else "Ctrl+C 或关闭两个窗口结束。"
    print(f"项目：{path}\n左：{args.comparator}；右：最新版 wallpaperd\n日志：{root}\n{ending}", flush=True)
    loop_index = 0
    if args.loop:
        print(f"轮播 {args.loop}：共 {len(paths)} 项，每项加载后展示 {args.duration}s。\n[1/{len(paths)}] {path}", flush=True)
    try:
        sides = ((args.comparator, args.reference, reference_backend, factory),
                 ("wallpaperd", args.wallpaperd, "rust", Session))
        for name, binary, backend, session_type in sides:
            options = {"wine_config": args.wine_config} if session_type is WineSession else {}
            sessions.append(session_type(name, binary.resolve(), root, runtime, cache, parent, properties, stopped,
                                    args.bench, backend, fps=args.fps, **options))
            if wine_mode and name == "wallpaperd":
                config = sessions[-1].root / "config/misari/wallpaperd.toml"
                config.write_text("media_integration = false\n" + config.read_text())
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(s.compositor, args.niri.resolve()) for s in sessions]
            for future in futures:
                future.result()
            windows = wait_for(lambda: find_windows(args.niri, parent, sessions), stopped)
            logical = focused["logical"]
            width = min(960, (logical["width"] - 64) // 2)
            height = min(width * 9 // 16, logical["height"] - 96)
            for index, window in enumerate(windows):
                wid = str(window["id"])
                for action in (("move-window-to-floating",), ("set-window-width", str(width)),
                               ("set-window-height", str(height)), ("move-floating-window", "--x",
                                str(24 + index * (width + 16)), "--y", str((logical["height"] - height) // 2))):
                    command([args.niri, "msg", "action", action[0], "--id", wid, *action[1:]], parent)
            # Wait for both clients to acknowledge equal size before starting render workers.
            def resized():
                current = find_windows(args.niri, parent, sessions)
                return len(current) == 2 and all(w["layout"]["window_size"] == [width, height]
                                                  for w in current)

            wait_for(resized, stopped)
            futures = [pool.submit(s.play, path) for s in sessions]
            for future in futures:
                future.result()
        if args.bench:
            if any(s.state.get("error") for s in sessions):
                raise RuntimeError("bench 要求两边都加载成功")
            sizes = []
            for session in sessions:
                output = json.loads(command([args.niri, "msg", "--json", "outputs"], session.env))["winit"]
                mode = output["modes"][output["current_mode"]]
                sizes.append((mode["width"], mode["height"]))
            if sizes[0] != sizes[1]:
                raise RuntimeError(f"bench 两边渲染分辨率不一致：{sizes}")
            width, height = sizes[0]

        def write_sessions():
            manifest = root / "sessions.json"
            pending = manifest.with_suffix(".tmp")
            pending.write_text(json.dumps({"runtime": str(runtime),
                "loop": {"group": args.loop, "duration_s": args.duration,
                         "projects": list(map(str, paths)), "current": str(path)} if args.loop else None,
                "bench": {"duration_s": args.duration, "warmup_s": args.warmup, "sample_s": 1, "resolution": [width, height], "target_fps": args.fps,
                          "scope": "Wine: complete systemd cgroup; wallpaperd: process tree; excludes nested niri",
                          "fps_source": "Wayland buffer commits (Wine capture submissions, not official renderer FPS)"}
                         if args.bench else None, "sessions": [
                {"name": s.name, "binary": str(s.binary), "env": {k: s.env[k] for k in
                 ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
                  "WAYLAND_DISPLAY", "NIRI_SOCKET", "PULSE_SERVER")},
                 "pids": [p.pid for p in s.children], "state": s.state} for s in sessions]}, indent=2))
            pending.replace(manifest)

        write_sessions()
        print(f"两边加载结束，耗时 {time.monotonic() - started:.1f}s。", flush=True)
        if args.bench:
            return benchmark(sessions, root, stopped, width, height, args.duration,
                             warmup=args.warmup, project=path)
        next_switch = time.monotonic() + args.duration
        while not stopped.wait(0.2):
            for session in sessions:
                if session.niri.poll() is not None:
                    session.close()
            if all(s.niri.poll() is not None for s in sessions):
                break
            if args.loop:
                live = [s for s in sessions if s.niri.poll() is None]
                if any(s.daemon.poll() is not None for s in live):
                    raise RuntimeError("轮播期间 wallpaperd 退出，请查看日志")
                if time.monotonic() >= next_switch:
                    loop_index = (loop_index + 1) % len(paths)
                    path = paths[loop_index]
                    print(f"[{loop_index + 1}/{len(paths)}] {path}", flush=True)
                    with ThreadPoolExecutor(max_workers=2) as pool:
                        futures = [pool.submit(s.play, path) for s in live]
                        for session, future in zip(live, futures):
                            try:
                                future.result()
                            except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                                if session.niri.poll() is None:
                                    raise
                                session.close()
                    write_sessions()
                    next_switch = time.monotonic() + args.duration
        return 0
    except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
        if stopped.is_set():
            return 0
        raise
    finally:
        stopped.set()
        for session in sessions:
            session.close()
        shutil.rmtree(runtime)


def find_windows(niri, parent, sessions):
    windows = json.loads(command([niri, "msg", "--json", "windows"], parent))
    owned = [next((w for w in windows if w.get("pid") == s.niri.pid), None) for s in sessions]
    return owned if all(owned) else []


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"对比启动失败：{error}", file=sys.stderr)
        sys.exit(1)
