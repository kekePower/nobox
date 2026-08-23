#!/usr/bin/env python3
"""Verify that separately shipped backend executables stay dependency-isolated."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def packages(root: Path, package: str) -> set[str]:
    output = subprocess.run(
        [
            "cargo",
            "tree",
            "--locked",
            "--package",
            package,
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout
    return {line.split(" v", 1)[0] for line in output.splitlines() if line}


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: backend-dependency-boundaries.py SOURCE_DIR")
    root = Path(sys.argv[1]).resolve()
    x11 = packages(root, "nobox-x11")
    wayland = packages(root, "nobox-wayland")
    common = packages(root, "nobox-common")

    forbidden_x11 = {name for name in x11 if name == "nobox-wayland" or name.startswith("smithay")}
    if forbidden_x11:
        raise SystemExit(f"X11 backend reaches Wayland/Smithay: {sorted(forbidden_x11)}")
    if "nobox-x11" in wayland:
        raise SystemExit("Wayland backend reaches the X11 window-manager crate")
    forbidden_common = {"nobox-x11", "nobox-wayland"} & common
    if forbidden_common:
        raise SystemExit(f"common session support reaches a backend: {sorted(forbidden_common)}")

    print("backend dependency boundaries passed")


if __name__ == "__main__":
    main()
