#!/usr/bin/python3
"""Controllable process-boundary fixture for manager helper failures."""

from __future__ import annotations

import json
import sys
from pathlib import Path


mode = Path(__file__).with_name("semantic-helper-mode").read_text(encoding="utf-8").strip()

if mode == "crash":
    raise SystemExit(17)
if mode == "truncate":
    sys.stdout.write('{"v":1,"status":"matched"')
    raise SystemExit(0)
if mode == "oversize":
    sys.stdout.write("x" * (1024 * 1024 + 1))
    raise SystemExit(0)
if mode == "unavailable":
    sys.stdout.write('{"v":1,"status":"unavailable"}')
    raise SystemExit(0)
if mode != "matched":
    raise SystemExit(2)

request = json.load(sys.stdin)
rect = request["rects"][0]
response = {
    "v": 1,
    "status": "matched",
    "root": {
        "id": 1,
        "role": "window",
        "name": "semantic recovery fixture",
        "states": ["visible"],
        "bounds": {
            "x": 0,
            "y": 0,
            "width": rect["width"],
            "height": rect["height"],
        },
        "child_count": 0,
    },
}
sys.stdout.write(json.dumps(response, separators=(",", ":")))
