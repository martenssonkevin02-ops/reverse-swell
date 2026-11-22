"""macOS GUI smoke test: plays a stab through the plugin so it captures a swell, opens the
plugin window, screenshots it, then drags the captured swell onto the Desktop with synthetic
mouse events and checks that Finder received the WAV file.

Usage: python3 ci/mac_gui_test.py "path/to/Reverse Swell.vst3" <screenshot_dir>
"""
import glob
import os
import subprocess
import sys
import threading
import time

import numpy as np
import Quartz
from pedalboard import load_plugin

SR = 48000
out_dir = sys.argv[2]
os.makedirs(out_dir, exist_ok=True)

p = load_plugin(sys.argv[1])
n = int(0.7 * SR)
t = np.arange(n) / SR
stab = sum((2 * ((t * f) % 1.0) - 1) for f in (220.0, 261.63, 329.63)) * np.exp(-t * 2.2) * 0.15
x = np.concatenate([np.zeros(int(1.0 * SR)), stab, np.zeros(int(1.5 * SR))]).astype(np.float32)
p.process(np.stack([x, x]), SR, buffer_size=512, reset=True)

close = threading.Event()
result = {}


def shot(name):
    subprocess.run(["screencapture", "-x", os.path.join(out_dir, name)], check=False)


def plugin_window():
    pid = os.getpid()
    info = Quartz.CGWindowListCopyWindowInfo(Quartz.kCGWindowListOptionOnScreenOnly, Quartz.kCGNullWindowID)
    wins = [w for w in info if w.get("kCGWindowOwnerPID") == pid and w.get("kCGWindowLayer", 1) == 0]
    wins.sort(key=lambda w: -w["kCGWindowBounds"]["Width"] * w["kCGWindowBounds"]["Height"])
    return wins[0]["kCGWindowBounds"] if wins else None


def post(kind, x, y):
    ev = Quartz.CGEventCreateMouseEvent(None, kind, (x, y), Quartz.kCGMouseButtonLeft)
    Quartz.CGEventPost(Quartz.kCGHIDEventTap, ev)


def driver():
    try:
        time.sleep(5)
        shot("1_window.png")
        b = plugin_window()
        result["window"] = b
        print("plugin window:", b)
        if b:
            # The capture waveform sits near the bottom of the 600x600 editor
            title = b["Height"] - 600
            sx, sy = b["X"] + 200, b["Y"] + title + 547
            # Drop on an empty Desktop spot in the bottom-right corner, well clear of the window
            screen = Quartz.CGDisplayBounds(Quartz.CGMainDisplayID())
            dx, dy = screen.size.width - 160, screen.size.height - 160
            print(f"screen {screen.size.width:.0f}x{screen.size.height:.0f}, dragging from ({sx}, {sy}) to ({dx}, {dy})")
            post(Quartz.kCGEventMouseMoved, sx, sy)
            time.sleep(0.5)
            post(Quartz.kCGEventLeftMouseDown, sx, sy)
            time.sleep(0.2)
            steps = 40
            for i in range(1, steps + 1):
                post(Quartz.kCGEventLeftMouseDragged, sx + (dx - sx) * i / steps, sy + (dy - sy) * i / steps)
                time.sleep(0.03)
                if i == steps // 2:
                    shot("2_dragging.png")
            time.sleep(0.4)
            post(Quartz.kCGEventLeftMouseUp, dx, dy)
            time.sleep(3)
            shot("3_after_drop.png")
    finally:
        close.set()


threading.Thread(target=driver, daemon=True).start()
p.show_editor(close)

saved = sorted(glob.glob(os.path.expanduser("~/Music/Reverse Swell/*.wav")))
dropped = sorted(glob.glob(os.path.expanduser("~/Desktop/Swell*.wav")))
print("saved by plugin:", saved)
print("dropped on Desktop:", dropped)
if not saved:
    print("FAILED: dragging didn't write the WAV")
    sys.exit(1)
if not dropped:
    print("WARNING: Finder didn't receive the drop (synthetic events may be blocked on CI)")
    sys.exit(2)
print("drag-out works")
