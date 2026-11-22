"""Loads the built plugin in a real VST3 host (Spotify's pedalboard) and checks the audio.

Usage: python3 ci/host_test.py "path/to/Reverse Swell.vst3"
Exits non-zero if anything is off.
"""
import sys
import time

import numpy as np
from pedalboard import load_plugin

SR = 48000
ms = lambda v: int(v * SR / 1000)


def stab(seconds, gain=0.5):
    n = int(seconds * SR)
    t = np.arange(n) / SR
    env = np.minimum(t / 0.002, 1.0) * np.exp(-t * 2.2)
    out = np.zeros(n)
    for f in (220.0, 261.63, 329.63):
        for d in (-0.11, -0.06, -0.02, 0.0, 0.02, 0.06, 0.11):
            freq = f * 2 ** (d / 12)
            out += (2 * ((t * freq + d) % 1.0) - 1) * env
    return (out / np.max(np.abs(out)) * gain).astype(np.float32)


x = np.concatenate([np.zeros(int(2.0 * SR), np.float32), stab(0.7), np.zeros(int(1.5 * SR), np.float32)])
x = np.stack([x, x])
onset = int(2.0 * SR)
trigger = onset + int(np.argmax(np.abs(x[0, onset:]) >= 10 ** (-30 / 20)))

p = load_plugin(sys.argv[1])
print("loaded:", p.name, "| params:", len(p.parameters))
failures = []


def settle():
    for _ in range(3):
        p.process(np.zeros((2, 8192), np.float32), SR, buffer_size=512, reset=False)
        time.sleep(0.25)


def check(label, swell_ms):
    settle()
    y = p.process(x, SR, buffer_size=512, reset=True)
    lat = p.reported_latency_samples
    post = np.max(np.abs(y[0][trigger + ms(5):trigger + ms(100)] - x[0][trigger + ms(5):trigger + ms(100)]))
    swell = y[:, trigger - ms(swell_ms):trigger]
    peak_db = 20 * np.log10(np.max(np.abs(swell)) + 1e-12)
    before = np.max(np.abs(y[:, :trigger - ms(swell_ms) - ms(5)]))
    print(f"{label}: latency {lat / SR * 1000:.0f} ms | post-hit mismatch {post:.1e} | "
          f"swell peak {peak_db:.1f} dBFS | before swell {before:.1e}")
    if abs(lat / SR * 1000 - swell_ms) > 2:
        failures.append(f"{label}: latency {lat} doesn't match swell length")
    if post > 1e-4:
        failures.append(f"{label}: dry not aligned after latency compensation")
    if not (-20 < peak_db < 0):
        failures.append(f"{label}: swell level off ({peak_db:.1f} dBFS)")
    if before > 1e-4:
        failures.append(f"{label}: output before the swell window")


p.tempo_sync = False
p.length_ms = "801 ms"
check("801 ms", 801)
p.length_ms = "1.50 s"
check("1.5 s", 1500)
p.length_ms = "300 ms"
p.curve = 6.0
check("300 ms, curve 6", 300)

if failures:
    print("FAILED:\n  " + "\n  ".join(failures))
    sys.exit(1)
print("all audio checks passed")
