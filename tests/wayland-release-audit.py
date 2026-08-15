#!/usr/bin/env python3
"""Static release-boundary audit for the native Wayland backend."""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from pathlib import Path


EXPECTED_SMITHAY_FEATURES = {
    "backend_drm",
    "backend_gbm",
    "backend_libinput",
    "backend_session_libseat",
    "backend_udev",
    "backend_winit",
    "desktop",
    "renderer_gl",
    "renderer_multi",
    "renderer_pixman",
    "use_system_lib",
    "wayland_frontend",
}
UNSAFE_DECLARATION = re.compile(r"\bunsafe\s+(?:extern|fn|impl|trait)|\bunsafe\s*\{")
TRACING_CALL = re.compile(
    r"\b(?:trace|debug|info|warn|error)!\((.*?)\);", re.DOTALL
)
LOG_SECRET_FIELD = re.compile(
    r"\b(?:title|class|app_id|token|desktop_entry|command|payload|pixels)\s*(?:=|,)"
)
STRING_LITERAL = re.compile(r'"(?:\\.|[^"\\])*"')


def fail(message: str) -> None:
    raise SystemExit(f"wayland release audit: {message}")


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: wayland-release-audit.py SOURCE_DIR")
    root = Path(sys.argv[1]).resolve()
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    smithay = manifest["workspace"]["dependencies"]["smithay"]
    features = set(smithay["features"])
    if smithay.get("default-features") is not False:
        fail("Smithay default features must remain disabled")
    if features != EXPECTED_SMITHAY_FEATURES:
        fail(f"Smithay feature drift: {sorted(features)}")

    patch = manifest["patch"]["crates-io"]["smithay"]
    if patch != {
        "git": "https://github.com/Smithay/smithay.git",
        "rev": "2b285e2d2d5ecbabea249906c36ef20fe4c6808d",
    }:
        fail(f"unreviewed Smithay source override: {patch}")

    for source in sorted((root / "crates").glob("**/*.rs")):
        text = source.read_text(encoding="utf-8")
        if UNSAFE_DECLARATION.search(text):
            fail(f"unsafe Nobox Rust in {source.relative_to(root)}")

    for source in sorted((root / "crates/nobox-wayland/src").glob("**/*.rs")):
        text = source.read_text(encoding="utf-8")
        for call in TRACING_CALL.findall(text):
            fields = STRING_LITERAL.sub("", call)
            if LOG_SECRET_FIELD.search(fields):
                fail(f"sensitive tracing field in {source.relative_to(root)}")

    session = (root / "data/nobox-wayland.desktop").read_text(encoding="utf-8")
    if "Exec=nobox --backend wayland run --tty\n" not in session:
        fail("installed Wayland session does not select the explicit TTY backend")

    tree = subprocess.run(
        [
            "cargo",
            "tree",
            "--locked",
            "--package",
            "nobox-wayland",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}\t{l}",
        ],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.splitlines()
    missing = [line.split("\t", 1)[0] for line in tree if not line.partition("\t")[2]]
    if missing:
        fail(f"dependencies without declared licenses: {sorted(set(missing))}")

    print(
        "Wayland source audit passed: exact Smithay features/source, no unsafe "
        f"Nobox Rust, redacted logs, explicit session entry, {len(set(tree))} licensed records"
    )


if __name__ == "__main__":
    main()
