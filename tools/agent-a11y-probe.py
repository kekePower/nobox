#!/usr/bin/env python3
"""Bounded AT-SPI root-correlation prototype for Agent Seat v2 B4.

The process accepts one JSON request on stdin and emits one compact JSON
result on stdout.  It deliberately reads no accessible names, descriptions,
attributes, values, text, or descendants.  It is a discovery experiment, not
a shipped helper or a public protocol endpoint.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from dataclasses import dataclass
from typing import Any


VERSION = 1
MAX_INPUT_BYTES = 16 * 1024
MAX_TARGET_PIDS = 64
MAX_TARGET_RECTS = 2
MAX_DESKTOPS = 4
MAX_APPLICATIONS = 64
MAX_TOPLEVELS = 64
MAX_SLOT_MS = 2_000
DEFAULT_SLOT_MS = 1_000
DEFAULT_CALL_MS = 150
# Qt 6 exposes a plain top-level QWidget as FILLER directly beneath its
# application root.  Direct-child position plus correlation evidence, rather
# than the role alone, is what makes it a top-level candidate.
TOPLEVEL_ROLES = frozenset({"dialog", "filler", "frame", "window"})


class InvalidRequest(ValueError):
    """The manager-to-probe request violated the prototype contract."""


@dataclass(frozen=True)
class Rect:
    """A screen-coordinate rectangle used only inside the helper boundary."""

    x: int
    y: int
    width: int
    height: int


@dataclass(frozen=True)
class Target:
    """Manager-supplied evidence for one already-authorized client."""

    pids: frozenset[int]
    rects: frozenset[Rect]
    single_client: bool


@dataclass(frozen=True)
class Candidate:
    """Minimal evidence collected for one accessible top-level."""

    pids: frozenset[int]
    rect: Rect
    role: str
    showing: bool
    visible: bool
    defunct: bool


def _object(value: Any, *, keys: frozenset[str], path: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise InvalidRequest(f"{path}:object")
    if frozenset(value) != keys:
        raise InvalidRequest(f"{path}:keys")
    return value


def _integer(value: Any, *, minimum: int, maximum: int, path: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise InvalidRequest(f"{path}:integer")
    if not minimum <= value <= maximum:
        raise InvalidRequest(f"{path}:range")
    return value


def _boolean(value: Any, *, path: str) -> bool:
    if not isinstance(value, bool):
        raise InvalidRequest(f"{path}:boolean")
    return value


def _array(value: Any, *, minimum: int, maximum: int, path: str) -> list[Any]:
    if not isinstance(value, list):
        raise InvalidRequest(f"{path}:array")
    if not minimum <= len(value) <= maximum:
        raise InvalidRequest(f"{path}:length")
    return value


def _rect(value: Any, *, path: str) -> Rect:
    item = _object(
        value,
        keys=frozenset({"x", "y", "width", "height"}),
        path=path,
    )
    return Rect(
        x=_integer(item["x"], minimum=-(2**31), maximum=2**31 - 1, path=f"{path}/x"),
        y=_integer(item["y"], minimum=-(2**31), maximum=2**31 - 1, path=f"{path}/y"),
        width=_integer(item["width"], minimum=1, maximum=65_535, path=f"{path}/width"),
        height=_integer(item["height"], minimum=1, maximum=65_535, path=f"{path}/height"),
    )


def parse_target(value: Any) -> Target:
    """Validate a strict, bounded manager request."""

    request = _object(
        value,
        keys=frozenset({"v", "pids", "rects", "single_client"}),
        path="",
    )
    if _integer(request["v"], minimum=VERSION, maximum=VERSION, path="/v") != VERSION:
        raise InvalidRequest("/v:version")
    raw_pids = _array(
        request["pids"], minimum=1, maximum=MAX_TARGET_PIDS, path="/pids"
    )
    pids = frozenset(
        _integer(pid, minimum=1, maximum=2**31 - 1, path=f"/pids/{index}")
        for index, pid in enumerate(raw_pids)
    )
    if len(pids) != len(raw_pids):
        raise InvalidRequest("/pids:duplicate")
    raw_rects = _array(
        request["rects"], minimum=1, maximum=MAX_TARGET_RECTS, path="/rects"
    )
    rects = frozenset(_rect(rect, path=f"/rects/{index}") for index, rect in enumerate(raw_rects))
    if len(rects) != len(raw_rects):
        raise InvalidRequest("/rects:duplicate")
    return Target(
        pids=pids,
        rects=rects,
        single_client=_boolean(request["single_client"], path="/single_client"),
    )


def parse_candidates(value: Any) -> list[Candidate]:
    """Validate deterministic candidate fixtures without loading AT-SPI."""

    raw_candidates = _array(value, minimum=0, maximum=MAX_TOPLEVELS, path="/candidates")
    candidates: list[Candidate] = []
    keys = frozenset({"pids", "rect", "role", "showing", "visible", "defunct"})
    for index, value in enumerate(raw_candidates):
        path = f"/candidates/{index}"
        item = _object(value, keys=keys, path=path)
        raw_pids = _array(
            item["pids"], minimum=1, maximum=2, path=f"{path}/pids"
        )
        pids = frozenset(
            _integer(pid, minimum=1, maximum=2**31 - 1, path=f"{path}/pids/{pid_index}")
            for pid_index, pid in enumerate(raw_pids)
        )
        if len(pids) != len(raw_pids):
            raise InvalidRequest(f"{path}/pids:duplicate")
        role = item["role"]
        if not isinstance(role, str) or role not in TOPLEVEL_ROLES:
            raise InvalidRequest(f"{path}/role:enum")
        candidates.append(
            Candidate(
                pids=pids,
                rect=_rect(item["rect"], path=f"{path}/rect"),
                role=role,
                showing=_boolean(item["showing"], path=f"{path}/showing"),
                visible=_boolean(item["visible"], path=f"{path}/visible"),
                defunct=_boolean(item["defunct"], path=f"{path}/defunct"),
            )
        )
    return candidates


def correlate(target: Target, candidates: list[Candidate], *, complete: bool) -> str:
    """Return a fail-closed correlation status with no candidate details."""

    if not complete:
        return "unavailable"
    eligible: list[Candidate] = []
    for candidate in candidates:
        if (
            candidate.pids.issubset(target.pids)
            and candidate.role in TOPLEVEL_ROLES
            and candidate.showing
            and candidate.visible
            and not candidate.defunct
        ):
            eligible.append(candidate)

    exact = [candidate for candidate in eligible if candidate.rect in target.rects]
    if len(exact) == 1:
        return "matched"
    if len(exact) > 1:
        return "ambiguous"

    # Some bridges report client-local (0,0) after the WM has placed the X11
    # window.  Ignoring position is safe only after the manager has counted
    # exactly one live top-level for the complete process family and the
    # helper independently sees exactly one live accessible top-level.
    if target.single_client and len(eligible) == 1:
        candidate = eligible[0]
        if any(
            candidate.rect.width == rect.width and candidate.rect.height == rect.height
            for rect in target.rects
        ):
            return "matched"
    return "ambiguous" if len(eligible) > 1 else "unavailable"


def discover(target: Target, call_ms: int) -> tuple[list[Candidate], bool]:
    """Read only the minimal AT-SPI evidence needed for root correlation."""

    try:
        import gi

        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi

        Atspi.set_timeout(call_ms, call_ms)
        desktop_count = Atspi.get_desktop_count()
        if not 0 <= desktop_count <= MAX_DESKTOPS:
            return [], False

        candidates: list[Candidate] = []
        application_count = 0
        role_names = {
            int(Atspi.Role.DIALOG): "dialog",
            int(Atspi.Role.FILLER): "filler",
            int(Atspi.Role.FRAME): "frame",
            int(Atspi.Role.WINDOW): "window",
        }
        for desktop_index in range(desktop_count):
            desktop = Atspi.get_desktop(desktop_index)
            child_count = desktop.get_child_count()
            if child_count < 0 or application_count + child_count > MAX_APPLICATIONS:
                return [], False
            application_count += child_count
            for app_index in range(child_count):
                app = desktop.get_child_at_index(app_index)
                app_pid = app.get_process_id()
                if app_pid not in target.pids:
                    continue
                top_count = app.get_child_count()
                if top_count < 0 or top_count > MAX_TOPLEVELS:
                    return [], False
                for top_index in range(top_count):
                    top = app.get_child_at_index(top_index)
                    role = role_names.get(int(top.get_role()))
                    if role is None:
                        continue
                    top_pid = top.get_process_id()
                    if top_pid <= 0:
                        return [], False
                    state = top.get_state_set()
                    component = top.get_component_iface()
                    if component is None:
                        continue
                    extents = component.get_extents(Atspi.CoordType.SCREEN)
                    candidates.append(
                        Candidate(
                            pids=frozenset({app_pid, top_pid}),
                            rect=Rect(
                                x=extents.x,
                                y=extents.y,
                                width=extents.width,
                                height=extents.height,
                            ),
                            role=role,
                            showing=state.contains(Atspi.StateType.SHOWING),
                            visible=state.contains(Atspi.StateType.VISIBLE),
                            defunct=state.contains(Atspi.StateType.DEFUNCT),
                        )
                    )
                    if len(candidates) > MAX_TOPLEVELS:
                        return [], False
        return candidates, True
    except Exception:
        return [], False


def _read_json() -> Any:
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES or not raw:
        raise InvalidRequest(":size")
    try:
        return json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InvalidRequest(":json") from error


def _write(status: str, started: float, slot_ms: int) -> None:
    remaining = started + slot_ms / 1_000 - time.monotonic()
    if remaining > 0:
        time.sleep(remaining)
    sys.stdout.write(json.dumps({"v": VERSION, "status": status}, separators=(",", ":")))
    sys.stdout.write("\n")
    sys.stdout.flush()


def main() -> int:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--fixture", action="store_true")
    parser.add_argument("--slot-ms", type=int, default=DEFAULT_SLOT_MS)
    parser.add_argument("--call-ms", type=int, default=DEFAULT_CALL_MS)
    args = parser.parse_args()
    started = time.monotonic()
    if not 0 <= args.slot_ms <= MAX_SLOT_MS or not 1 <= args.call_ms <= 1_000:
        _write("invalid", started, 0)
        return 2
    try:
        value = _read_json()
        if args.fixture:
            envelope = _object(
                value,
                keys=frozenset({"target", "candidates", "complete"}),
                path="",
            )
            target = parse_target(envelope["target"])
            candidates = parse_candidates(envelope["candidates"])
            complete = _boolean(envelope["complete"], path="/complete")
        else:
            target = parse_target(value)
            candidates, complete = discover(target, args.call_ms)
        status = correlate(target, candidates, complete=complete)
    except InvalidRequest:
        status = "invalid"
    _write(status, started, args.slot_ms)
    return 0 if status != "invalid" else 2


if __name__ == "__main__":
    raise SystemExit(main())
