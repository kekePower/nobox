#!/usr/bin/env python3
"""Drive a nobox agent seat end to end through the real MCP companion.

This is the flow a harness actually performs: discover, subscribe, launch and
identify without looking at pixels, act across a workspace boundary, be refused
for acting on a stale belief, perform one bounded action-and-observation, look
at pixels only where pixels are the answer, and be preempted by the person.

Nothing here knows anything nobox-specific beyond the socket path and the
fixture desktop entry: it speaks MCP to a subprocess.
"""

import json
import subprocess
import sys
import time

VERSION = "2026-07-28"
META = {
    "io.modelcontextprotocol/protocolVersion": VERSION,
    "io.modelcontextprotocol/clientCapabilities": {},
    "io.modelcontextprotocol/clientInfo": {"name": "nobox-flow-test", "version": "1"},
}


class Companion:
    """A stock MCP host, as far as the companion can tell."""

    def __init__(self, command: str, socket: str) -> None:
        self.process = subprocess.Popen(
            [command, "--socket", socket],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 1

    def request(self, method: str, params: dict | None = None) -> dict:
        identity = self.next_id
        self.next_id += 1
        payload = dict(params or {})
        payload["_meta"] = META
        line = json.dumps(
            {"jsonrpc": "2.0", "id": identity, "method": method, "params": payload}
        )
        assert self.process.stdin is not None
        assert self.process.stdout is not None
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()
        answer = self.process.stdout.readline()
        if not answer:
            raise SystemExit(f"the companion stopped answering after {method}")
        message = json.loads(answer)
        if message.get("id") != identity:
            raise SystemExit(f"expected an answer to {identity}, got {message}")
        return message

    def call(self, tool: str, arguments: dict | None = None) -> dict:
        message = self.request(
            "tools/call", {"name": tool, "arguments": arguments or {}}
        )
        if "error" in message:
            raise SystemExit(f"{tool} failed at the protocol level: {message['error']}")
        result = message["result"]
        assert result["resultType"] == "complete", result
        return result

    def ok(self, tool: str, arguments: dict | None = None) -> dict:
        result = self.call(tool, arguments)
        if result.get("isError"):
            raise SystemExit(f"{tool} was refused: {result['content']}")
        return result["structuredContent"]

    def refusal(self, tool: str, arguments: dict | None = None) -> dict:
        result = self.call(tool, arguments)
        if not result.get("isError"):
            raise SystemExit(f"{tool} unexpectedly succeeded: {result}")
        return result.get("structuredContent", {})

    def close(self) -> None:
        assert self.process.stdin is not None
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()


def press(press_key: str, *arguments: str) -> None:
    """Acts as the person at the keyboard."""
    subprocess.run([press_key, *arguments], check=False, capture_output=True)


def poll_for(companion: Companion, after: int, predicate, timeout: float = 20.0):
    """Follows the event stream from a cursor until something matches."""
    deadline = time.monotonic() + timeout
    sequence = after
    while time.monotonic() < deadline:
        answer = companion.ok("events_poll", {"after_seq": sequence, "wait_ms": 2000})
        for envelope in answer["events"]:
            sequence = max(sequence, envelope["sequence"])
            if envelope["event"].get("event") == "resync_required":
                raise SystemExit("the event stream overflowed during the flow")
            found = predicate(envelope["event"])
            if found is not None:
                return found, sequence
        sequence = max(sequence, answer["sequence"])
    raise SystemExit("nothing matching arrived on the event stream in time")


def main(companion_binary: str, socket: str, press_key: str, entry: str) -> int:
    companion = Companion(companion_binary, socket)
    try:
        # 1. Static discovery is fast; the explicit status tool reaches the seat.
        discover = companion.request("server/discover")["result"]
        assert discover["supportedVersions"][0] == VERSION, discover
        assert "2025-11-25" in discover["supportedVersions"], discover
        instructions = discover["instructions"]
        assert len(instructions.encode("utf-8")) <= 1_000, instructions
        assert "desktop_snapshot" in instructions, instructions
        status = companion.ok("seat_status", {})["status"]
        assert "Granted:" in status, status
        print(f"1. discovered: {status.split('Connected to')[-1].strip()}")

        listing = companion.request("tools/list")["result"]
        tools = {tool["name"] for tool in listing["tools"]}
        for required in (
            "seat_status",
            "desktop_subscribe",
            "events_poll",
            "launch",
            "client_semantic_root",
            "client_semantic_tree",
            "client_semantic_find",
            "client_pointer",
        ):
            assert required in tools, tools

        # Invalid parameters explain the exact mechanical correction. Repair
        # the named field and retry without reading diagnostic prose.
        malformed = companion.request(
            "tools/call",
            {
                "name": "events_poll",
                "arguments": {"after_seq": 0, "wait_ms": "brief"},
            },
        )
        correction = malformed["error"]["data"]
        assert malformed["error"]["code"] == -32602, malformed
        assert correction["code"] == "invalid_argument", correction
        assert correction["path"] == "/wait_ms", correction
        assert correction["expected"]["kind"] == "integer", correction
        assert correction["received"] == "string", correction
        assert correction["retryable"] == "after_correction", correction
        companion.ok("events_poll", {"after_seq": 0, "wait_ms": 0})

        # 2. One world model, and the stream that keeps it true.
        subscribed = companion.ok("desktop_subscribe", {})
        snapshot = subscribed["snapshot"]
        sequence = snapshot["sequence"]
        print(f"2. subscribed at sequence {sequence} with {len(snapshot['clients'])} windows")

        # 3. Launch, and identify what it opened without a single pixel.
        token = companion.ok("launch", {"desktop_entry": entry})["launch"]
        mapped, sequence = poll_for(
            companion,
            sequence,
            lambda event: event["client"]
            if event.get("event") == "client_mapped" and event.get("launch") == token
            else None,
        )
        client = mapped["client"]
        print(f"3. launched {entry} and correlated window {client}")

        # 4. Put it on another workspace, then activate it from here.
        companion.ok(
            "client_send_to_workspace", {"client": client, "workspace": 1, "follow": False}
        )
        committed = companion.ok("client_activate", {"client": client})["committed"]
        assert "workspace_switch" in committed, committed
        assert "activate" in committed, committed
        print(f"4. activated across a workspace boundary: {committed}")

        # 5. Act only on what is still true.
        described = companion.ok("client_get", {"client": client})["client"]
        stale = companion.refusal(
            "client_pointer",
            {
                "client": client,
                "x": 5,
                "y": 5,
                "action": "click",
                "button": "left",
                "expects": {"generation": described["generation"] - 1},
            },
        )
        assert stale["code"] == "stale_state", stale
        assert stale["retryable"] == "after_observation", stale
        fresh = companion.ok("client_get", {"client": client})["client"]
        observed = companion.call(
            "client_pointer",
            {
                "client": client,
                "x": 5,
                "y": 5,
                "action": "click",
                "ensure_visible": True,
                "expects": {"generation": fresh["generation"]},
                "observe": {
                    "capture": {"rect": {"x": 0, "y": 0, "width": 80, "height": 80}},
                    "minimum_ms": 50,
                    "quiet_ms": 100,
                    "maximum_ms": 1000,
                },
            },
        )
        assert observed["isError"] is False, observed
        assert observed["content"][0]["type"] == "image", observed
        injected = observed["structuredContent"]
        committed = injected["committed"]
        assert committed[-1] == "inject", committed
        assert injected["delivery"] == "unverified", injected
        assert injected["action"] >= 1, injected
        observation = injected["observation"]
        assert 50 <= observation["elapsed_ms"] <= 1500, observation
        assert observation["dropped_events"] == 0, observation
        sample = observation["samples"][0]
        assert sample["status"] == "ok", sample
        assert "data" not in sample["image"], sample
        print(
            f"5. refused a stale click, then observed action {injected['action']} "
            f"for {observation['elapsed_ms']}ms"
        )

        event_only = companion.ok(
            "client_key",
            {
                "client": client,
                "key": "PageUp",
                "action": "tap",
                "observe": {
                    "minimum_ms": 25,
                    "quiet_ms": 50,
                    "maximum_ms": 500,
                },
            },
        )
        assert event_only["observation"]["samples"] == [], event_only

        # 6. Pixels, where only pixels answer.
        captured = companion.call(
            "client_capture", {"client": client, "grid": {"spacing": 100}}
        )
        assert captured["isError"] is False, captured
        assert captured["content"][0]["type"] == "image", captured
        assert captured["content"][0]["mimeType"] == "image/png", captured
        image = captured["structuredContent"]["image"]
        assert image["format"] == "png", image
        assert image["width"] == fresh["content"]["width"], (image, fresh)
        assert image["grid"] == {"spacing": 100, "origin_x": 0, "origin_y": 0}, image
        assert "data" not in image, image
        print(f"6. captured {image['width']}x{image['height']} at sequence {image['sequence']}")

        # 7. The person at the keyboard wins, and can stop everything.
        press(press_key, "--plain", "a")
        interrupted = companion.refusal(
            "client_pointer",
            {"client": client, "x": 5, "y": 5, "action": "click", "button": "left"},
        )
        assert interrupted["code"] == "interrupted", interrupted
        assert interrupted["retryable"] == "after_human_idle", interrupted
        print("7. human input preempted the agent")

        press(press_key, "--control", "--alt", "Escape")
        time.sleep(0.5)
        frozen = companion.refusal("desktop_snapshot", {})
        assert frozen["code"] == "session_frozen", frozen
        assert frozen["retryable"] == "after_session_resume", frozen
        press(press_key, "--control", "--alt", "Escape")
        time.sleep(0.5)
        companion.ok("desktop_snapshot", {})
        print("8. the kill chord froze the session, and resuming restored its grant")
        return 0
    finally:
        companion.close()


if __name__ == "__main__":
    if len(sys.argv) != 5:
        print(
            "usage: agent-mcp-flow.py NOBOX_AGENT SOCKET PRESS_KEY DESKTOP_ENTRY",
            file=sys.stderr,
        )
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]))
