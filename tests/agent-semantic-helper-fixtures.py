#!/usr/bin/env python3
"""Process-boundary checks for the disposable Rust accessibility helper."""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any


HELPER = Path(sys.argv[1]).resolve()
sys.argv = [sys.argv[0]]
RECT = {"x": 40, "y": 70, "width": 900, "height": 600}
TARGET = {"v": 1, "pids": [100, 101], "rects": [RECT], "single_client": False}


def candidate(**changes: Any) -> dict[str, Any]:
    value = {
        "pids": [100],
        "rect": RECT,
        "role": "frame",
        "showing": True,
        "visible": True,
        "defunct": False,
    }
    value.update(changes)
    return value


class HelperFixtures(unittest.TestCase):
    def invoke(self, value: Any) -> tuple[int, dict[str, Any], str]:
        process = subprocess.run(
            [str(HELPER), "--fixture"],
            input=json.dumps(value),
            text=True,
            capture_output=True,
            timeout=2,
            check=False,
        )
        return process.returncode, json.loads(process.stdout), process.stderr

    def test_unique_exact_and_bijection_results(self) -> None:
        code, result, stderr = self.invoke(
            {"target": TARGET, "candidates": [candidate()], "complete": True}
        )
        self.assertEqual((code, result, stderr), (0, {"v": 1, "status": "matched"}, ""))

        stale = candidate(rect={**RECT, "x": 0, "y": 0})
        _, result, _ = self.invoke(
            {
                "target": {**TARGET, "single_client": True},
                "candidates": [stale],
                "complete": True,
            }
        )
        self.assertEqual(result["status"], "matched")

    def test_missing_unrelated_and_partial_are_byte_equivalent(self) -> None:
        values = [
            [],
            [candidate(pids=[999])],
            [candidate(visible=False)],
        ]
        results = [
            self.invoke({"target": TARGET, "candidates": value, "complete": True})[1]
            for value in values
        ]
        results.append(
            self.invoke(
                {"target": TARGET, "candidates": [candidate()], "complete": False}
            )[1]
        )
        self.assertEqual(results, [{"v": 1, "status": "unavailable"}] * 4)

    def test_unknown_content_fields_and_oversized_input_are_invalid(self) -> None:
        hostile = candidate(name="secret")
        code, result, stderr = self.invoke(
            {"target": TARGET, "candidates": [hostile], "complete": True}
        )
        self.assertEqual((code, result, stderr), (2, {"v": 1, "status": "invalid"}, ""))

        process = subprocess.run(
            [str(HELPER), "--fixture"],
            input="x" * (16 * 1024 + 1),
            text=True,
            capture_output=True,
            timeout=2,
            check=False,
        )
        self.assertEqual(process.returncode, 2)
        self.assertEqual(json.loads(process.stdout), {"v": 1, "status": "invalid"})
        self.assertEqual(process.stderr, "")


if __name__ == "__main__":
    unittest.main()
