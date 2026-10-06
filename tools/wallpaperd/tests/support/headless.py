"""Private Weston lifecycle shared by checks and benchmarks."""
from contextlib import contextmanager
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


@contextmanager
def weston(log_path=None, *, size=(1600, 1000), shell="desktop-shell.so"):
    with tempfile.TemporaryDirectory(prefix="wd-headless-") as directory:
        runtime = Path(directory)
        runtime.chmod(0o700)
        env = dict(os.environ, XDG_RUNTIME_DIR=directory, WAYLAND_DISPLAY="test-parent")
        # Repeated libmpv thread creation otherwise makes the RSS assertion depend
        # on glibc's CPU-count-based arena growth rather than live resources.
        env.setdefault("MALLOC_ARENA_MAX", "2")
        for key in ("WAYLAND_SOCKET", "DISPLAY", "NIRI_SOCKET"):
            env.pop(key, None)
        with (runtime / "weston.log").open("w") as log:
            parent = subprocess.Popen(
                ["weston", "--backend=headless", "--renderer=gl", "--no-config",
                 "--idle-time=0", "--socket=test-parent", f"--width={size[0]}",
                 f"--height={size[1]}", f"--shell={shell}"],
                env=env, stdout=log, stderr=log,
            )
            try:
                deadline = time.monotonic() + 10
                while not (runtime / "test-parent").exists():
                    if parent.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError((runtime / "weston.log").read_text())
                    time.sleep(0.05)
                yield env
            finally:
                parent.terminate()
                try:
                    parent.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    parent.kill()
                    parent.wait()
                if log_path:
                    shutil.copyfile(runtime / "weston.log", log_path)
