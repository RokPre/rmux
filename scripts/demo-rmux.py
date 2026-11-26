#!/usr/bin/env python3
"""Record a short rmux feature demo in a dedicated i3 workspace.

Usage: python3 scripts/demo-rmux.py [output.gif]

Requires i3, st (with the rmux key-encoding patch), ffmpeg, xclip, and
python-xlib with XTEST support. It types brief `echo` captions before major
actions, uses rmux's documented default bindings, and shows split dividers /
resizing rather than individual outlines. The script leaves its unique session
and workspace open; it never kills the session.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from pathlib import Path

try:
    from Xlib import X, XK, display
    from Xlib.ext import xtest
except ImportError as exc:
    print("demo needs python-xlib with XTEST support", file=sys.stderr)
    raise SystemExit(1) from exc


ROOT = Path(__file__).resolve().parents[1]
RMUX = Path(os.environ.get("RMUX_BIN", ROOT / "target/release/rmux")).expanduser()
ST = os.environ.get("ST_BIN", "st")
SESSION = f"rmux-demo-{datetime.now():%Y%m%d-%H%M%S}-{os.getpid()}"
WORKSPACE = SESSION


def executable(value: str | Path) -> str:
    text = str(value)
    found = shutil.which(text) if "/" not in text else text
    if not found or not Path(found).is_file():
        raise RuntimeError(f"required executable not found: {text}")
    return str(Path(found).resolve())


def run_i3(command: str) -> None:
    result = subprocess.run(
        ["i3-msg", "-t", "command", command], capture_output=True, text=True
    )
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"i3 failed for {command!r}: {detail}")
    replies = json.loads(result.stdout)
    if isinstance(replies, dict):
        replies = [replies]
    failures = [reply for reply in replies if not reply.get("success", False)]
    if failures:
        raise RuntimeError(f"i3 rejected {command!r}: {failures}")


def i3_tree() -> dict:
    result = subprocess.run(
        ["i3-msg", "-t", "get_tree"], check=True, capture_output=True, text=True
    )
    return json.loads(result.stdout)


def window_ids() -> set[int]:
    found: set[int] = set()

    def visit(node: dict) -> None:
        window = node.get("window")
        if isinstance(window, int):
            found.add(window)
        for key in ("nodes", "floating_nodes"):
            for child in node.get(key, []):
                visit(child)

    visit(i3_tree())
    return found


def workspaces() -> list[dict]:
    result = subprocess.run(
        ["i3-msg", "-t", "get_workspaces"], check=True, capture_output=True, text=True
    )
    return json.loads(result.stdout)


def active_outputs() -> list[dict]:
    result = subprocess.run(
        ["i3-msg", "-t", "get_outputs"], check=True, capture_output=True, text=True
    )
    return [output for output in json.loads(result.stdout) if output["active"]]


def preferred_output() -> dict:
    outputs = active_outputs()
    if not outputs:
        raise RuntimeError("i3 reports no active outputs")
    return max(
        outputs,
        key=lambda item: (
            item["rect"]["width"] >= item["rect"]["height"],
            item["rect"]["width"] * item["rect"]["height"],
        ),
    )


def visible_workspace(output_name: str) -> str:
    return next(
        workspace["name"]
        for workspace in workspaces()
        if workspace["output"] == output_name and workspace["visible"]
    )


def focused_workspace() -> str:
    return next(workspace["name"] for workspace in workspaces() if workspace["focused"])


def capture_rect(rect: dict, root_width: int, root_height: int) -> dict[str, int]:
    x = rect["x"] - (1 << 32) if rect["x"] >= (1 << 31) else rect["x"]
    y = rect["y"] - (1 << 32) if rect["y"] >= (1 << 31) else rect["y"]
    left, top = max(0, x), max(0, y)
    right = min(root_width, x + rect["width"])
    bottom = min(root_height, y + rect["height"])
    if right <= left or bottom <= top:
        raise RuntimeError(f"demo output is outside the X11 screen: {rect}")
    return {"x": left, "y": top, "width": right - left, "height": bottom - top}


def wait_for_window(previous: set[int], description: str) -> int:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        added = window_ids() - previous
        if added:
            return max(added)
        time.sleep(0.1)
    raise RuntimeError(f"timed out waiting for {description}")


def launch_client(st: str, rmux: str, args: list[str]) -> int:
    previous = window_ids()
    command = shlex.join(
        [
            st,
            "-V",
            "-c",
            "demo-terminal",
            "-e",
            "env",
            "SHELL=/bin/sh",
            "ENV=/dev/null",
            "PS1=demo$ ",
            rmux,
            *args,
        ]
    )
    run_i3(f"exec --no-startup-id {command}")
    window = wait_for_window(previous, "st window")
    run_i3(f"[id={window}] move container to workspace {WORKSPACE}")
    run_i3(f"[id={window}] focus")
    time.sleep(0.8)
    return window


def write_demo_shell(path: Path) -> None:
    path.write_text(
        "#!/bin/sh\n"
        "i=1\n"
        "while [ \"$i\" -le 140 ]; do\n"
        "  case \"$i\" in\n"
        "    60|70|80|90) printf 'conversation %03d: searchable needle in retained output\\n' \"$i\" ;;\n"
        "    *) printf 'conversation %03d: example terminal output\\n' \"$i\" ;;\n"
        "  esac\n"
        "  i=$((i + 1))\n"
        "done\n"
        "printf '\\nrmux demo history ready\\n'\n"
        "unset ENV\n"
        "PS1='demo$ '\n"
        "export PS1\n"
        "exec /bin/sh -i\n",
        encoding="utf-8",
    )


class Keyboard:
    def __init__(self) -> None:
        self.display = display.Display()
        if not self.display.has_extension("XTEST"):
            raise RuntimeError("the X server does not provide the XTEST extension")

    def keycode(self, name: str) -> int:
        keysym = XK.string_to_keysym(name)
        keycode = self.display.keysym_to_keycode(keysym) if keysym else 0
        if not keycode:
            raise RuntimeError(f"no X11 keycode for {name!r}")
        return keycode

    def tap(self, name: str, delay: float = 0.12) -> None:
        code = self.keycode(name)
        xtest.fake_input(self.display, X.KeyPress, code)
        xtest.fake_input(self.display, X.KeyRelease, code)
        self.display.sync()
        time.sleep(delay)

    def chord(self, modifiers: tuple[str, ...], key: str) -> None:
        modifier_codes = [self.keycode(name) for name in modifiers]
        key_code = self.keycode(key)
        for code in modifier_codes:
            xtest.fake_input(self.display, X.KeyPress, code)
        xtest.fake_input(self.display, X.KeyPress, key_code)
        xtest.fake_input(self.display, X.KeyRelease, key_code)
        for code in reversed(modifier_codes):
            xtest.fake_input(self.display, X.KeyRelease, code)
        self.display.sync()
        time.sleep(0.16)

    def type_demo_text(self, text: str) -> None:
        if not text.isascii() or any(
            character != " "
            and not ("a" <= character <= "z" or "0" <= character <= "9")
            for character in text
        ):
            raise ValueError("demo text supports lowercase ASCII, digits, and spaces")
        for character in text:
            self.tap("space" if character == " " else character, delay=0.06)

    def close(self) -> None:
        self.display.close()


def narrate(keyboard: Keyboard, text: str) -> None:
    keyboard.chord(("Control_L",), "u")
    keyboard.type_demo_text(f"echo {text}")
    keyboard.tap("Return")
    time.sleep(1.0)


def rename_buffer(keyboard: Keyboard, name: str) -> None:
    keyboard.chord(("Control_L",), "r")
    keyboard.chord(("Control_L",), "u")
    keyboard.type_demo_text(name)
    keyboard.tap("Return")
    time.sleep(0.8)

def zoom_st(keyboard: Keyboard) -> None:
    for _ in range(12):
        keyboard.chord(("Control_L",), "plus")


def focus_upper_right(keyboard: Keyboard) -> None:
    """From the full-height left pane, resolve the two right-side choices."""
    for direction in ("l", "j", "k"):
        keyboard.chord(("Alt_L",), direction)


def stop_recording(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.send_signal(signal.SIGINT)
    try:
        status = process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
        raise RuntimeError("ffmpeg did not stop after the recording was interrupted")
    if status not in (0, 255):
        raise RuntimeError(f"ffmpeg recording failed with exit status {status}")


def main() -> int:
    if len(sys.argv) > 2:
        print(f"usage: {sys.argv[0]} [output.gif]", file=sys.stderr)
        return 2
    if not os.environ.get("DISPLAY"):
        raise RuntimeError("DISPLAY is not set; run this from the active X11 session")

    rmux = executable(RMUX)
    st = executable(ST)
    for command in ("i3-msg", "ffmpeg", "xclip"):
        executable(command)

    output = (
        Path(sys.argv[1]).expanduser()
        if len(sys.argv) == 2
        else Path.cwd() / f"rmux-demo-{datetime.now():%Y%m%d-%H%M%S}.gif"
    )
    output = output.resolve()
    if output.exists():
        raise RuntimeError(f"refusing to overwrite existing file: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)

    old_workspace = focused_workspace()
    demo_output = preferred_output()
    demo_output_name = demo_output["name"]
    previous_output_workspace = visible_workspace(demo_output_name)
    keyboard = Keyboard()
    screen = keyboard.display.screen()
    temporary = tempfile.TemporaryDirectory(prefix="rmux-demo-")
    shell_script = Path(temporary.name) / "demo-shell.sh"
    capture = Path(temporary.name) / "capture.mkv"
    write_demo_shell(shell_script)

    recorder: subprocess.Popen | None = None
    output_focused = False
    try:
        run_i3(f"focus output {demo_output_name}")
        output_focused = True
        run_i3(f"workspace {WORKSPACE}")
        time.sleep(0.7)
        rect = capture_rect(demo_output["rect"], screen.width_in_pixels, screen.height_in_pixels)
        width, height = rect["width"], rect["height"]
        first = launch_client(st, rmux, ["new", SESSION, "--", "/bin/sh", str(shell_script)])
        zoom_st(keyboard)
        time.sleep(1.0)
        recorder = subprocess.Popen(
            [
                "ffmpeg",
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "x11grab",
                "-framerate",
                "12",
                "-video_size",
                f"{width}x{height}",
                "-i",
                f"{os.environ['DISPLAY']}+{rect['x']},{rect['y']}",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "28",
                "-pix_fmt",
                "yuv420p",
                str(capture),
            ],
            stdin=subprocess.DEVNULL,
        )
        time.sleep(1)


        narrate(keyboard, "next name history")
        rename_buffer(keyboard, "history")

        narrate(keyboard, "next scroll terminal history")
        for _ in range(3):
            keyboard.chord(("Alt_L",), "d")
            time.sleep(0.7)
        for _ in range(3):
            keyboard.chord(("Alt_L",), "f")
            time.sleep(0.7)
        keyboard.tap("i")
        time.sleep(0.5)


        run_i3(f"[id={first}] focus")
        narrate(keyboard, "next split rmux left and right")
        keyboard.chord(("Alt_L",), "v")
        time.sleep(0.8)
        narrate(keyboard, "next split the right side top and bottom")
        keyboard.chord(("Alt_L",), "b")
        time.sleep(0.8)
        narrate(keyboard, "next focus upper right")
        keyboard.chord(("Alt_L",), "k")
        time.sleep(0.6)

        narrate(keyboard, "next create guide buffer")
        keyboard.chord(("Control_L",), "t")
        time.sleep(0.9)
        narrate(keyboard, "next rename it guide")
        rename_buffer(keyboard, "guide")

        narrate(keyboard, "next move guide tab left")
        keyboard.chord(("Control_L", "Shift_L"), "h")
        time.sleep(0.7)
        narrate(keyboard, "next move guide tab right")
        keyboard.chord(("Control_L", "Shift_L"), "l")
        time.sleep(0.7)

        narrate(keyboard, "next focus lower right")
        keyboard.chord(("Alt_L",), "j")
        time.sleep(0.6)
        narrate(keyboard, "next create notes buffer")
        keyboard.chord(("Control_L",), "t")
        time.sleep(0.9)
        narrate(keyboard, "next rename it notes")
        rename_buffer(keyboard, "notes")
        narrate(keyboard, "three panes show three buffers")

        narrate(keyboard, "next create fourth buffer")
        keyboard.chord(("Control_L",), "t")
        time.sleep(0.9)
        narrate(keyboard, "next name and park scratch")
        rename_buffer(keyboard, "scratch")
        narrate(keyboard, "next restore history below")
        for _ in range(3):
            keyboard.chord(("Control_L",), "h")
            time.sleep(0.55)

        narrate(keyboard, "history in left and lower right")
        narrate(keyboard, "next resize shared history")
        for _ in range(2):
            keyboard.chord(("Alt_L", "Control_L"), "l")
            time.sleep(0.5)

        narrate(keyboard, "next navigate between rmux panes")
        keyboard.chord(("Alt_L",), "k")
        time.sleep(0.6)
        keyboard.chord(("Alt_L",), "j")
        time.sleep(0.6)
        keyboard.chord(("Alt_L",), "h")
        time.sleep(0.6)
        narrate(keyboard, "vim keys and colors are configurable")

        narrate(keyboard, "next search printed history for needle")
        keyboard.chord(("Control_L",), "f")
        keyboard.type_demo_text("needle")
        time.sleep(1.0)
        keyboard.tap("Return")
        time.sleep(1.4)

        focus_upper_right(keyboard)
        narrate(keyboard, "next step through matching results")
        keyboard.chord(("Alt_L",), "h")
        keyboard.tap("comma")
        time.sleep(0.9)
        keyboard.tap("semicolon")
        time.sleep(0.9)

        focus_upper_right(keyboard)
        narrate(keyboard, "next select the match characterwise")
        keyboard.chord(("Alt_L",), "h")
        keyboard.tap("v")
        for _ in range(5):
            keyboard.tap("l")
        time.sleep(0.8)

        focus_upper_right(keyboard)
        narrate(keyboard, "next switch to whole line selection")
        keyboard.chord(("Alt_L",), "h")
        keyboard.chord(("Shift_L",), "v")
        time.sleep(0.8)

        focus_upper_right(keyboard)
        narrate(keyboard, "next switch to block selection")
        keyboard.chord(("Alt_L",), "h")
        keyboard.chord(("Control_L",), "v")
        keyboard.tap("j")
        for _ in range(6):
            keyboard.tap("l")
        time.sleep(0.8)

        focus_upper_right(keyboard)
        narrate(keyboard, "next yank the selected block")
        keyboard.chord(("Alt_L",), "h")
        keyboard.tap("y")
        time.sleep(1.0)
        narrate(keyboard, "next attach second st client")
        run_i3("split h")
        second = launch_client(st, rmux, ["attach", SESSION])
        zoom_st(keyboard)
        narrate(keyboard, "next resize the st clients")
        run_i3(f"[id={second}] resize set width 38 ppt")
        time.sleep(1.0)
        run_i3(f"[id={first}] focus")

        focus_upper_right(keyboard)
        narrate(keyboard, "next close the first st client")
        run_i3(f"[id={first}] kill")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and first in window_ids():
            time.sleep(0.1)
        if first in window_ids():
            raise RuntimeError("the first st client did not close")
        time.sleep(0.7)

        narrate(keyboard, "next attach a third client to this session")
        run_i3("split h")
        third = launch_client(st, rmux, ["attach", SESSION])
        zoom_st(keyboard)
        run_i3(f"[id={third}] resize set width 42 ppt")
        time.sleep(1.0)
        narrate(keyboard, "the same rmux session is still live")
        time.sleep(1.2)

        stop_recording(recorder)
        recorder = None
        subprocess.run(
            [
                "ffmpeg",
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-n",
                "-i",
                str(capture),
                "-filter_complex",
                "fps=8,setpts=PTS/1.5,scale=1200:-1:flags=lanczos,split[a][b];"
                "[a]palettegen=max_colors=64[p];[b][p]paletteuse=dither=bayer",
                "-fps_mode",
                "passthrough",
                "-loop",
                "0",
                str(output),
            ],
            check=True,
        )
    finally:
        try:
            stop_recording(recorder)
        finally:
            try:
                if output_focused:
                    if previous_output_workspace != old_workspace:
                        run_i3(f"workspace {previous_output_workspace}")
                    run_i3(f"workspace {old_workspace}")
            finally:
                keyboard.close()
                temporary.cleanup()

    print(f"GIF: {output}")
    print(f"Demo session: {SESSION}")
    print(f"Demo workspace: {WORKSPACE}")
    print(f"Reattach with: {rmux} attach {SESSION}")
    print(f"Demo output: {demo_output_name}")
    print("The demo clients and session are left open for inspection.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError, StopIteration) as exc:
        print(f"demo failed: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
