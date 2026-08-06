#!/usr/bin/env python3
"""Deterministic fixture coverage for the B4 AT-SPI root probe."""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any


PROBE = Path(sys.argv[1]).resolve()
sys.argv = [sys.argv[0]]
RECT = {"x": 40, "y": 70, "width": 900, "height": 600}
TARGET = {
    "v": 1,
    "pids": [100, 101],
    "rects": [RECT],
    "single_client": False,
}


def candidate(**changes: Any) -> dict[str, Any]:
    value: dict[str, Any] = {
        "pids": [100],
        "rect": RECT,
        "role": "frame",
        "showing": True,
        "visible": True,
        "defunct": False,
    }
    value.update(changes)
    return value


class ProbeFixtures(unittest.TestCase):
    def run_probe(
        self,
        candidates: list[dict[str, Any]],
        *,
        complete: bool = True,
        target: dict[str, Any] = TARGET,
    ) -> tuple[int, dict[str, Any]]:
        process = subprocess.run(
            [sys.executable, str(PROBE), "--fixture", "--slot-ms", "0"],
            input=json.dumps(
                {"target": target, "candidates": candidates, "complete": complete}
            ),
            text=True,
            capture_output=True,
            check=False,
            timeout=2,
        )
        self.assertEqual(process.stderr, "")
        return process.returncode, json.loads(process.stdout)

    def test_unique_gtk_or_qt_root_matches(self) -> None:
        code, result = self.run_probe([candidate()])
        self.assertEqual(code, 0)
        self.assertEqual(result, {"v": 1, "status": "matched"})

        _, qt = self.run_probe([candidate(role="filler")])
        self.assertEqual(qt["status"], "matched")

    def test_browser_process_family_matches(self) -> None:
        code, result = self.run_probe([candidate(pids=[100, 101])])
        self.assertEqual(code, 0)
        self.assertEqual(result["status"], "matched")

    def test_single_client_bijection_tolerates_stale_origin(self) -> None:
        target = {**TARGET, "single_client": True}
        stale_origin = candidate(
            rect={"x": 0, "y": 0, "width": RECT["width"], "height": RECT["height"]}
        )
        _, result = self.run_probe([stale_origin], target=target)
        self.assertEqual(result["status"], "matched")

    def test_positionless_match_requires_bijection(self) -> None:
        target = {**TARGET, "single_client": True}
        stale_origin = candidate(
            rect={"x": 0, "y": 0, "width": RECT["width"], "height": RECT["height"]}
        )
        _, result = self.run_probe(
            [stale_origin, {**stale_origin, "role": "window"}], target=target
        )
        self.assertEqual(result["status"], "ambiguous")

    def test_unrelated_process_is_indistinguishable_from_no_root(self) -> None:
        _, absent = self.run_probe([])
        _, unrelated = self.run_probe([candidate(pids=[999])])
        self.assertEqual(absent, unrelated)
        self.assertEqual(absent["status"], "unavailable")

    def test_unrelated_geometry_is_indistinguishable_from_no_root(self) -> None:
        _, absent = self.run_probe([])
        _, unrelated = self.run_probe(
            [candidate(rect={"x": 41, "y": 70, "width": 900, "height": 600})]
        )
        self.assertEqual(absent, unrelated)

    def test_stale_hidden_and_defunct_roots_fail_closed(self) -> None:
        for changes in (
            {"showing": False},
            {"visible": False},
            {"defunct": True},
        ):
            with self.subTest(changes=changes):
                _, result = self.run_probe([candidate(**changes)])
                self.assertEqual(result["status"], "unavailable")

    def test_duplicate_exact_roots_are_ambiguous(self) -> None:
        _, result = self.run_probe([candidate(), candidate(role="window")])
        self.assertEqual(result["status"], "ambiguous")

    def test_partial_scan_cannot_match(self) -> None:
        _, result = self.run_probe([candidate()], complete=False)
        self.assertEqual(result["status"], "unavailable")

    def test_title_never_enters_the_contract(self) -> None:
        invalid = candidate()
        invalid["name"] = "out-of-scope text"
        code, result = self.run_probe([invalid])
        self.assertEqual(code, 2)
        self.assertEqual(result["status"], "invalid")

    def test_request_bounds_and_unknown_fields_are_strict(self) -> None:
        target = {**TARGET, "x11_window": 42}
        code, result = self.run_probe([], target=target)
        self.assertEqual(code, 2)
        self.assertEqual(result, {"v": 1, "status": "invalid"})


if __name__ == "__main__":
    unittest.main()
