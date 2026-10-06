#!/usr/bin/env python3
"""Run prepared wallpaper runtimes; --bench measures the five schemes sequentially."""
import argparse
import csv
from contextlib import closing, nullcontext
from datetime import datetime
import json
import math
import hashlib
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import threading
import tomllib

import compare

ROOT = Path(__file__).resolve().parents[1]
REPO = ROOT.parents[1]
RUNTIMES = REPO / "3rd/wallpaper/runtime"
STEAM = Path.home() / "Share/Steam"
ASSETS = STEAM / "steamapps/common/wallpaper_engine/assets"
PROJECTS = STEAM / "steamapps/workshop/content/431960"
BINARIES = {
    "wine": REPO / "3rd/wallpaper/desktop/we-layerd-wine/target/release/we-layerd",
    "we-layerd": RUNTIMES / "we-layerd-native/squashfs-root/AppRun",
    "linux-wallpaperengine": REPO / "3rd/wallpaper/engines/linux-wallpaperengine/build/output/linux-wallpaperengine",
    "waywallen": RUNTIMES / "waywallen/squashfs-root/AppRun",
    "wallpaperd": ROOT / "target/we-web/release/wallpaperd",
}
PROJECT_IDS = {"scene": "3354366708", "video": "2103778541", "web": "3399006050"}
OUTPUT_SIZE = (3840, 2160)
SCHEMES = ("wine", "linux-wallpaperengine", "waywallen", "we-layerd", "wallpaperd")


def waywallen_rpc(port):
    import websocket
    from google.protobuf import descriptor_pb2, descriptor_pool, json_format, message_factory
    descriptors = descriptor_pb2.FileDescriptorSet.FromString((RUNTIMES / "waywallen/control.pb").read_bytes())
    pool = descriptor_pool.DescriptorPool()
    for descriptor in descriptors.file:
        pool.Add(descriptor)
    request_type = message_factory.GetMessageClass(pool.FindMessageTypeByName("waywallen.control.v1.Request"))
    frame_type = message_factory.GetMessageClass(pool.FindMessageTypeByName("waywallen.control.v1.ServerFrame"))

    def call(method, values=None):
        request = json_format.ParseDict({"request_id": 1, method: values or {}}, request_type())
        try:
            with closing(websocket.create_connection(f"ws://127.0.0.1:{port}", timeout=10,
                         http_no_proxy=["127.0.0.1"])) as ws:
                ws.send_binary(request.SerializeToString())
                while True:
                    frame = frame_type.FromString(ws.recv())
                    if frame.HasField("response"):
                        response = frame.response
                        break
        except websocket.WebSocketException as error:
            raise RuntimeError(f"Waywallen {method}: {error}") from error
        if response.request_id != 1 or response.error_code or response.status != 1:
            raise RuntimeError(f"Waywallen {method}: {response.message or response}")
        return response
    return call


def start_waywallen(session, project, fps):
    import dbus
    config = Path(session.env["XDG_CONFIG_HOME"]) / "waywallen/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text('[global]\naudio_capture_enabled = false\nmanual_muted = true\n'
        'plugin_update_notifications = false\n[global.renderer]\nenable_audio = false\nvolume = 0\n'
        '[global.auto_replay]\nfullscreen = "none"\nsession_locked = "none"\nsession_inactive = "none"\n' +
        ''.join(f'[plugin.{name}]\nfps = "{fps}"\nenable_audio = "false"\nvolume = "0"\n'
                'render_node = "/dev/dri/renderD128"\n'
                for name in ["wescene-renderer", "weweb-renderer", "waywallen-video"]))
    with (session.root / "dbus.log").open("w") as log:
        bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
            env=session.env, stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True)
    session.children.append(bus)
    if not select.select([bus.stdout], [], [], 5)[0]:
        raise RuntimeError("private D-Bus failed to start")
    address = bus.stdout.readline().strip()
    session.env["DBUS_SESSION_BUS_ADDRESS"] = address
    child = session.spawn([session.binary, "--no-ui", "--no-tray", "--no-restore",
        "--display-backend", "layer-shell", "--plugin", RUNTIMES / "waywallen"], "app.log")
    connection = dbus.bus.BusConnection(address)

    def port_ready():
        if child.poll() is not None:
            raise RuntimeError("Waywallen exited; see app.log")
        try:
            obj = connection.get_object("org.waywallen.waywallen.Daemon", "/org/waywallen/waywallen/Daemon", introspect=False)
            return int(dbus.Interface(obj, "org.freedesktop.DBus.Properties").Get(
                "org.waywallen.waywallen.Daemon1", "WsPort", timeout=2))
        except dbus.DBusException:
            return None
    port = compare.wait_for(port_ready, session.stopped, seconds=30)
    rpc = waywallen_rpc(port)
    compare.wait_for(lambda: any(s.name == "wallpaper_engine" for s in rpc("source_list").source_list.sources),
                     session.stopped, seconds=30)
    rpc("library_add", {"path": str(STEAM), "plugin_name": "wallpaper_engine"})
    rpc("wallpaper_scan")

    def found():
        entries = rpc("wallpaper_list").wallpaper_list.wallpapers
        return next((e for e in entries if e.external_id == project.name or e.resource.rstrip("/") == str(project)), None)
    entry = compare.wait_for(found, session.stopped, seconds=30)
    compare.wait_for(lambda: rpc("display_list").display_list.displays, session.stopped, seconds=30)
    rpc("wallpaper_apply", {"wallpaper_id": entry.id})
    return child, lambda: rpc("renderer_list")


def start(session, scheme, project, fps, shm=False):
    if scheme == "wallpaperd":
        config = Path(session.env["XDG_CONFIG_HOME"]) / "misari/wallpaperd.toml"
        config.write_text("media_integration = false\n" + config.read_text() +
                          f'assets = {json.dumps(str(ASSETS))}\n')
        session.play(project)
        if session.state.get("error"):
            raise RuntimeError(session.state["error"])
        return session.daemon, lambda: session.cli("status")
    if scheme == "wine":
        session.play(project)
        return session.daemon, lambda: tomllib.loads(compare.command([session.binary, "ctl", "status"], session.env))
    if scheme == "we-layerd":
        return compare.start_layerd(session, project, fps, shm), lambda: None
    if scheme == "linux-wallpaperengine":
        if json.loads((project / "project.json").read_text())["type"].lower() == "web":
            # This CEF build needs X11 even though the wallpaper output uses Wayland.
            with (session.root / "xwayland.log").open("w") as log:
                xwayland = subprocess.Popen(["Xwayland", "-rootless", "-displayfd", "1",
                    "-nolisten", "tcp", "-noreset"], env=session.env, stdout=subprocess.PIPE,
                    stderr=log, text=True, start_new_session=True)
            session.children.append(xwayland)
            if not select.select([xwayland.stdout], [], [], 10)[0]:
                raise RuntimeError("private Xwayland failed to start; see xwayland.log")
            display = xwayland.stdout.readline().strip()
            if not display.isdecimal():
                raise RuntimeError("private Xwayland exited; see xwayland.log")
            session.env["DISPLAY"] = f":{display}"
        return session.spawn([session.binary, "--assets-dir", ASSETS, "--fps", str(fps),
            "--silent", "--no-audio-processing", "--noautomute", "--no-fullscreen-pause",
            "--screen-root", "winit", "--bg", project, "--scaling", "fill"], "app.log"), lambda: None
    return start_waywallen(session, project, fps)


def screenshot(session, niri, name):
    destination = session.root / name
    compare.command([niri, "msg", "action", "screenshot-screen", "--show-pointer", "false",
                     "--path", destination], session.env, timeout=10)
    compare.wait_for(lambda: destination.exists(), session.stopped)
    return destination


def run_one(args, root=None):
    project = (PROJECTS / args.project if args.project.isdecimal() else Path(args.project)).resolve()
    if not (project / "project.json").is_file():
        raise RuntimeError(f"missing project.json: {project}")
    binary = BINARIES[args.scheme]
    if not binary.is_file():
        raise RuntimeError(f"runtime not prepared: {binary}")
    root = root or ROOT / "output/community" / datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    root.mkdir(parents=True, exist_ok=True)
    print(f"日志：{root}", flush=True)
    niri = REPO / "niri/target/release/niri"
    stopped = threading.Event()
    sys.path.insert(0, str(ROOT / "tests"))
    from support.headless import weston
    gpu = {"DRI_PRIME": "pci-0000_03_00_0", "MESA_VK_DEVICE_SELECT": "1002:7550",
           "MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE": "1", "DISABLE_LSFG": "1"}
    os.environ.update(gpu)
    os_env = dict(os.environ, **gpu)
    parent = (weston(root / "weston.log", size=OUTPUT_SIZE, shell="kiosk-shell.so")
              if args.headless else nullcontext(os_env))
    result = {"scheme": args.scheme, "project": str(project), "binary": str(binary),
              "fps_limit": args.fps, "argv": sys.argv, "headless": args.headless}
    with binary.open("rb") as stream:
        result["binary_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
    if args.scheme == "we-layerd":
        result["prefer_dmabuf"] = not args.shm
    with parent as env, tempfile.TemporaryDirectory(prefix="wd-community-") as runtime:
        env.update(gpu, XDG_SESSION_TYPE="wayland")
        kwargs = {}
        if args.scheme == "wine":
            kind = json.loads((project / "project.json").read_text())["type"].strip().lower()
            kwargs["wine_config"] = args.wine_config or compare.default_wine_config(kind)
            result["wine_config"] = str(kwargs["wine_config"].resolve())
        if args.scheme == "wallpaperd":
            kwargs["backend"] = "rust"
        session_type = compare.WineSession if args.scheme == "wine" else compare.Session
        session = session_type(args.scheme, binary, root, Path(runtime), root / "cache", env,
                               {}, stopped, fps=args.fps, bench=args.bench, **kwargs)
        if args.scheme == "wine":
            # systemd-run talks to the host user manager, not the private Weston runtime.
            session.service_env = dict(os_env)
            session.service_env.setdefault("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
            session.service_env.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path=/run/user/{os.getuid()}/bus")
            session.env["DBUS_SESSION_BUS_ADDRESS"] = session.service_env["DBUS_SESSION_BUS_ADDRESS"]
        session.env["XDG_DATA_HOME"] = str(session.root / "data")
        # Host PulseAudio is shared for media initialization; the wallpapers themselves stay muted.
        session.env["PULSE_SERVER"] = os.environ.get("PULSE_SERVER", f"unix:/run/user/{os.getuid()}/pulse/native")
        try:
            session.compositor(niri)
            if args.bench:
                session.env["WAYLAND_DEBUG"] = "client"
                if args.scheme not in ("wine", "wallpaperd"):
                    session.frame_log = session.root / "app.log"
            child, status = start(session, args.scheme, project, args.fps, args.shm)
            if args.duration:
                stopped.wait(args.warmup if args.bench else max(3, args.duration - 1))
                if child.poll() is not None:
                    raise RuntimeError(f"runtime exited with {child.returncode}; see app.log")
                first = screenshot(session, niri, "first.png")
                stopped.wait(1)
                second = screenshot(session, niri, "second.png")
                from PIL import Image, ImageChops, ImageStat
                with Image.open(first) as a, Image.open(second) as b:
                    result.update(resolution=list(a.size), pixel_stddev=ImageStat.Stat(a.convert("RGB")).stddev,
                                  frames_differ=ImageChops.difference(a.convert("RGB"), b.convert("RGB")).getbbox() is not None)
                if result["resolution"] != list(OUTPUT_SIZE):
                    raise RuntimeError(f"unexpected output resolution: {result['resolution']}")
                state = status()
                if state is not None:
                    from google.protobuf.json_format import MessageToDict
                    result["state"] = state if isinstance(state, dict) else MessageToDict(state, preserving_proto_field_name=True)
                result["nonuniform_pixels"] = max(result["pixel_stddev"]) > 3
                if not result["nonuniform_pixels"]:
                    result["display"] = "BLACK"
                    raise RuntimeError("no visible wallpaper pixels; see screenshots and app.log")
                result["display"] = "RUN"
                if args.bench:
                    session.daemon = child
                    compare.benchmark([session], session.root, stopped, *OUTPUT_SIZE,
                                      duration=int(args.duration), project=project)
                    result["csv"] = str(session.root / "bench.csv")
                    result["warmup_s"] = args.warmup
                    result["duration_s"] = args.duration
                print(f"{args.scheme}: {project.name} {result['display']}", flush=True)
            else:
                print("独立 niri 已启动；Ctrl+C 退出并清理。", flush=True)
                while session.niri.poll() is None and child.poll() is None:
                    stopped.wait(0.2)
                if child.poll() not in (None, 0):
                    raise RuntimeError(f"runtime exited with {child.returncode}; see logs")
        except KeyboardInterrupt:
            result["cancelled"] = True
            stopped.set()
        except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
            result["error"] = str(error)
            print(str(error), file=sys.stderr)
        finally:
            if args.scheme not in ("wine", "wallpaperd") and hasattr(session, "daemon"):
                del session.daemon
            session.close()
            destination = root / (f"{args.scheme}.json" if args.bench else "result.json")
            destination.write_text(json.dumps(result, ensure_ascii=False, indent=2))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scheme", nargs="?", choices=BINARIES)
    parser.add_argument("project", nargs="?", help="Workshop ID or project directory")
    parser.add_argument("--bench", action="store_true", help="measure all three projects and five schemes")
    parser.add_argument("--headless", action="store_true")
    parser.add_argument("--duration", type=float, help="sampling seconds, or smoke check wait")
    parser.add_argument("--warmup", type=float, default=15)
    parser.add_argument("--fps", type=int, default=60)
    parser.add_argument("--shm", action="store_true", help="we-layerd only: use SHM")
    parser.add_argument("--wine-config", type=Path,
                        help="override Wine configuration; default: GE for scene/video, system Wine for web")
    args = parser.parse_args()
    args.duration = args.duration if args.duration is not None else (30.0 if args.bench else 0.0)
    if not 1 <= args.fps <= 240 or not math.isfinite(args.duration) or args.duration < 0:
        parser.error("fps must be 1..240; duration must be nonnegative")
    if not math.isfinite(args.warmup) or args.warmup < 0:
        parser.error("warmup must be finite and nonnegative")
    if args.bench and (args.duration < 1 or not args.duration.is_integer()):
        parser.error("bench duration must be a positive integer")
    if args.shm and args.scheme != "we-layerd":
        parser.error("--shm is only available for we-layerd")
    if not args.bench:
        if not args.scheme or not args.project:
            parser.error("specify scheme and project, or use --bench")
        result = run_one(args)
        return int("error" in result or result.get("cancelled", False))
    root = ROOT / "output/benchmark" / datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    root.mkdir(parents=True)
    projects = [args.project] if args.project else list(PROJECT_IDS.values())
    runs, rows, names = [], [], {}
    for project_id in projects:
        project = PROJECTS / project_id if project_id.isdecimal() else Path(project_id)
        metadata = json.loads((project / "project.json").read_text())
        kind = metadata["type"].lower()
        names[kind] = metadata["title"]
        for scheme in ([args.scheme] if args.scheme else SCHEMES):
            options = argparse.Namespace(**dict(vars(args), scheme=scheme, project=str(project),
                shm=args.shm or (scheme == "we-layerd" and kind == "video")))
            print(f"\n[{kind}] {scheme} | {args.fps} fps", flush=True)
            result = run_one(options, root / kind)
            if result.get("csv"):
                with Path(result["csv"]).open() as stream:
                    for row in csv.DictReader(stream):
                        rows.append(dict(type=kind, backend=row["backend"],
                            interval_s=float(row["interval_s"]),
                            **{key: float(row[key]) if row[key] else None for key, _ in compare.METRICS}))
            result["type"] = kind
            runs.append(result)
            (root / "results.json").write_text(json.dumps({"runs": runs, "rows": rows}, ensure_ascii=False, indent=2))
            if result.get("cancelled"):
                return 1
    from contextlib import redirect_stdout
    with (root / "summary.txt").open("w") as stream, redirect_stdout(stream):
        compare.print_benchmark(rows, root / "results.json", *OUTPUT_SIZE, args.duration,
                                fps=args.fps, project_names=names)
    print((root / "summary.txt").read_text(), flush=True)
    print(f"结果：{root}", flush=True)
    return int(any("error" in result for result in runs))


if __name__ == "__main__":
    raise SystemExit(main())
