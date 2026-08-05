//! `nobox-agent` — an MCP companion for an Agent Seat Protocol socket.
//!
//! The companion is a translator, not an authority. It speaks MCP revision
//! 2026-07-28 on stdio toward an agent harness and the Agent Seat Protocol
//! toward the window manager, and it decides nothing: the manager validates
//! every request against the session's grant regardless of anything decided
//! here. Nothing in this process is a security boundary, and it is written so
//! that this is obvious.
//!
//! It is a reference client for any window manager that implements the socket,
//! not only nobox.

mod mcp;
mod seat;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use std::time::Duration;

use agent_seat_proto::{
    Bundle, Call, CaptureArea, ClientId, EventKind, Expects, GeometryRequest, KeyAction, Modifier,
    Outcome, OutputId, PointerButton, Rect, Sequence, StateChange, WorkspaceId,
};
use serde_json::{Map, Value, json};

use mcp::{
    INVALID_PARAMS, Incoming, METHOD_NOT_FOUND, error_object, error_response, result_response,
};
use seat::Seat;

/// How long a client may cache `tools/list` and `server/discover`.
const CACHE_TTL_MS: u64 = 300_000;

/// What this companion asks a manager for when it connects.
///
/// It exposes tools over every bundle, so it names every bundle rather than
/// asking narrowly and then failing at the first tool outside the request.
/// This is a request, not a claim: the manager grants what it decides to, and
/// a person answering a consent dialog can refuse any of it.
const REQUESTED_BUNDLES: [Bundle; 5] = Bundle::ALL;

/// What a model is told about working this seat, one line per line of output.
///
/// The advice is deliberately about judgement rather than syntax: the schemas
/// already carry the syntax. What a model cannot infer from a schema is that
/// structure beats screenshots here, that a refusal may be the person typing
/// rather than a bug, and that a window it cannot see may be one the user
/// chose to keep private.
const GUIDANCE: &[&str] = &[
    "This server is a window manager's agent seat: a controlled second seat on someone's live desktop, beside the person using it.",
    "",
    "WHEN TO USE THESE TOOLS",
    "- These tools are the user's real screen. They see what is on it, start applications on it, and move its pointer and keyboard. Nothing here is a sandbox, a simulation, or a description of a desktop somewhere else.",
    "- Reach for them whenever a request is about what is on screen or about operating a program through its interface: opening or closing an application, finding out what windows are open, reading what a window is showing, filling something in, clicking something, moving or resizing a window, or switching workspaces.",
    "- Requests like that rarely name this server, a window manager, or any tool. \"Open a terminal and run top\", \"what am I looking at\", \"close that window\", \"paste this into the browser\" are all requests for these tools. The user should not have to say which tools to use, any more than they would name a file-reading tool when asking about a file.",
    "- Prefer these tools over the shell for anything about the graphical session. Running a command starts a program somewhere; asking the window manager places it, names it, and lets you see and drive it afterwards.",
    "- The shell is still right for the things it is right for: files, builds, version control, anything with no window in it.",
    "",
    "HOW TO WORK",
    "- Start with desktop_snapshot, or desktop_subscribe if you will act more than once. Both return every window with an identity, application class, title, position, size, workspace, and state. This is exact and cheap; a screenshot is neither. Use structure for anything structure can answer: which windows exist, where they are, which is focused.",
    "- Use client_capture for what only pixels can answer: reading what an application is showing, finding where to click inside it, and checking that something you did actually happened. Those are not exceptions to preferring structure, they are the cases structure does not cover.",
    "- After desktop_subscribe, keep the highest sequence number you have applied and pass it to events_poll as after_seq. Events describe every change, so applying them keeps your model of the desktop exact without polling. A resync_required event means the backlog was dropped: take a fresh snapshot and carry on.",
    "- Windows are identified by the numeric `client` field, which is stable while the window lives. Each descriptor also carries a `generation` that changes whenever the window does.",
    "",
    "ACTING SAFELY",
    "- Before acting on something you looked at earlier, pass `expects` with the generation you saw. A stale_state refusal means the window changed under you: read it again with client_get and retry once against what is actually there. This is how you avoid clicking the wrong thing after a window moved.",
    "- Input is addressed to a window, never to the screen. Coordinates are relative to that window's own content rectangle, which the descriptor gives you. Set ensure_visible when the window may be on another workspace or behind others.",
    "- To find a point to click, capture the window and read the picture directly. A capture's pixels are in the same coordinates client_pointer takes, and image.content names which part of the window they are: add its x and y to a pixel position to get the point to aim at. For a whole-window capture that origin is (0, 0), so a feature at pixel (x, y) is simply at (x, y). Do not estimate from the window's size or position on screen, and do not estimate from a scaled rendering of the image: use image.width and image.height as the truth about its size.",
    "- Use client_type for text and client_key for shortcuts and editing keys.",
    "- Prefer client_activate, client_move_resize, and the other management tools over clicking a titlebar: they go through the window manager and say exactly what they changed.",
    "",
    "PUTTING TEXT INTO AN APPLICATION",
    "- Clicking a text field and typing into it is one operation with three steps, not two unrelated calls. Do all three: click the field with client_pointer, type with client_type, then capture and read the result before you act as if it worked.",
    "- The verification step is not optional and not a nicety. An input reply says `injected` with `delivery: unverified`, and it means exactly that: this window manager emitted the events at the display server and addressed them to that window. It does not know, and cannot know, whether a text box, a canvas, or a browser's content process accepted them. Only pixels can tell you that.",
    "- So a successful-looking input reply is not evidence the text arrived. If the capture shows nothing landed, the usual cause is that the click did not put the keyboard focus where you thought. Click a different point inside the same control and look again. Do not repeat the same call expecting a different reply, and do not report success you have not seen.",
    "- Verifying is cheap if you aim it. Pass `rect` to client_capture to get back a few hundred pixels around the point you clicked rather than the whole window; the reply's `content` tells you where that patch sits, so you can click again from it without re-capturing everything.",
    "- Web pages and toolkit widgets live below the window this protocol addresses. There is no way to name a button or a text field here; the window is the smallest thing you can aim at. That is a real limit, so lean on the picture.",
    "",
    "WAITING FOR SOMETHING TO FINISH",
    "- Events describe the desktop, not what is inside a window. A page finishing a request, a reply arriving, a document rendering: none of that moves a window, so nothing will be pushed to you and events_poll will stay quiet.",
    "- When you are waiting on something inside an application, wait a sensible interval and capture again, comparing against what you last saw. Say what you are waiting for rather than polling in a tight loop.",
    "",
    "WHEN YOU ARE REFUSED",
    "- interrupted means the person is using their keyboard or mouse right now. They have priority by design. Stop, tell the user what you were doing, and wait; do not retry in a loop.",
    "- session_frozen means the person pressed the kill chord to stop all agent activity. Stop acting entirely and say so. session_revoked means the grant was withdrawn: stop, and do not reconnect.",
    "- denied means this seat was never granted that capability. Do not work around it; tell the user which capability you needed so they can decide.",
    "- no_such_client means the window is gone, is outside your grant's scope, or is one the user marked private. All three look identical on purpose. Take a fresh snapshot, work with what is there, and do not try to discover it another way.",
    "- Refusals arrive as tool errors with a structured code, not as protocol failures. Read the code and act on it.",
    "",
    "CONTEXT",
    "- Everything you do is attributed to this session in the window manager's log. The person sees an indicator while this seat holds input or capture, and a highlight on any window you type into.",
    "- This is someone's real desktop. Prefer the least invasive tool that answers the question, say what you are about to do before doing it, and leave windows roughly as you found them.",
];

/// One MCP tool and the seat call it becomes.
struct ToolDefinition {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: fn() -> Value,
}

/// Every tool this companion exposes, in a fixed order.
///
/// The order is deliberate: the revision asks for deterministic `tools/list`
/// output so clients can cache it and model prompts stay stable.
const TOOLS: &[ToolDefinition] = &[
    ToolDefinition {
        name: "desktop_snapshot",
        title: "Desktop snapshot",
        description: "List everything open on the user's real screen right now: outputs, \
                      workspaces, stacking order, focus, and one descriptor per window with its \
                      application, title, position, and size. This is the first call for any \
                      request about what the user is looking at or which programs are running. \
                      Prefer it over screenshots; it is exact, cheap, and stamped with the \
                      sequence number it corresponds to. Windows the session was not granted, \
                      and windows the user marked hidden, are absent.",
        schema: || json!({ "type": "object", "additionalProperties": false }),
    },
    ToolDefinition {
        name: "desktop_subscribe",
        title: "Subscribe to desktop events",
        description: "Begin an event stream and return the snapshot it continues from, as one \
                      operation: no change can fall between the two. Apply events in sequence \
                      order to the returned snapshot to keep an exact world model without \
                      polling for state. Pass kinds to narrow the stream; session control and \
                      resync are always delivered.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": [
                                "client_mapped",
                                "client_closed",
                                "title_changed",
                                "focus_changed",
                                "state_changed",
                                "geometry_changed",
                                "workspace_switched",
                                "human_activity",
                            ],
                        },
                        "description": "Event kinds to deliver; omit for every kind",
                    },
                },
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "events_poll",
        title: "Poll desktop events",
        description: "Return events after a sequence number, waiting up to wait_ms for the \
                      first one. Pass the highest sequence you have applied as after_seq; the \
                      sequence is the only state you need to carry between calls. A \
                      resync_required event means the backlog was dropped and the world model \
                      must be rebuilt with desktop_snapshot.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "after_seq": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Highest sequence already applied",
                    },
                    "wait_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 30000,
                        "description": "How long to wait for the first event",
                    },
                },
                "required": ["after_seq"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_get",
        title: "Window details",
        description: "Return one window's descriptor by its identity, including its generation \
                      counter for later freshness checks. Answers 'no such client' identically \
                      for windows that do not exist, are out of scope, or are hidden.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Window identity from a snapshot",
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_activate",
        title: "Activate a window",
        description: "Bring a window to the user's attention through the window manager's own \
                      focus rules: switch to its workspace if needed, restore it if minimized, \
                      and focus it. Reports exactly which steps were committed.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "content": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "integer" },
                                    "y": { "type": "integer" },
                                    "width": { "type": "integer", "minimum": 0 },
                                    "height": { "type": "integer", "minimum": 0 },
                                },
                                "required": ["x", "y", "width", "height"],
                                "additionalProperties": false,
                            },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_close",
        title: "Close a window",
        description: "Ask a window to close through its own protocol, exactly as clicking its \
                      close button would. The application may refuse or prompt; there is no way \
                      to kill a window through this server.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "content": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "integer" },
                                    "y": { "type": "integer" },
                                    "width": { "type": "integer", "minimum": 0 },
                                    "height": { "type": "integer", "minimum": 0 },
                                },
                                "required": ["x", "y", "width", "height"],
                                "additionalProperties": false,
                            },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_move_resize",
        title: "Move or resize a window",
        description: "Change a window's position or size. Omitted fields are left alone, and \
                      the window manager applies the same constraints it applies to a user \
                      dragging the window.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "width": { "type": "integer", "minimum": 1 },
                    "height": { "type": "integer", "minimum": 1 },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "content": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "integer" },
                                    "y": { "type": "integer" },
                                    "width": { "type": "integer", "minimum": 0 },
                                    "height": { "type": "integer", "minimum": 0 },
                                },
                                "required": ["x", "y", "width", "height"],
                                "additionalProperties": false,
                            },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_set_state",
        title: "Change window state",
        description: "Minimize, maximize, fullscreen, shade, pin to every workspace, or change \
                      stacking preference. Omitted fields are left alone.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "minimized": { "type": "boolean" },
                    "maximized_horizontal": { "type": "boolean" },
                    "maximized_vertical": { "type": "boolean" },
                    "fullscreen": { "type": "boolean" },
                    "shaded": { "type": "boolean" },
                    "sticky": { "type": "boolean" },
                    "above": { "type": "boolean" },
                    "below": { "type": "boolean" },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "content": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "integer" },
                                    "y": { "type": "integer" },
                                    "width": { "type": "integer", "minimum": 0 },
                                    "height": { "type": "integer", "minimum": 0 },
                                },
                                "required": ["x", "y", "width", "height"],
                                "additionalProperties": false,
                            },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_send_to_workspace",
        title: "Send a window to a workspace",
        description: "Move a window to another workspace, optionally following it there.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "workspace": { "type": "integer", "minimum": 0 },
                    "follow": { "type": "boolean" },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "content": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "integer" },
                                    "y": { "type": "integer" },
                                    "width": { "type": "integer", "minimum": 0 },
                                    "height": { "type": "integer", "minimum": 0 },
                                },
                                "required": ["x", "y", "width", "height"],
                                "additionalProperties": false,
                            },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client", "workspace"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "workspace_switch",
        title: "Switch workspace",
        description: "Display another workspace.",
        schema: || {
            json!({
                "type": "object",
                "properties": { "workspace": { "type": "integer", "minimum": 0 } },
                "required": ["workspace"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "launch",
        title: "Open an application on the desktop",
        description: "Open an installed application on the user's desktop by its desktop-entry \
                      identifier, and return a correlation token. Use this to start a program \
                      the user wants to see and use — a terminal, a browser, an editor — rather \
                      than running its command in a shell, because the window it opens arrives \
                      as a client_mapped event carrying that token. Launch-and-identify is then \
                      one round trip and needs no guessing from titles. Only entries the user's \
                      launch policy allows can be started; there is no way to run an arbitrary \
                      command through this server.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "desktop_entry": {
                        "type": "string",
                        "maxLength": 256,
                        "description": "Desktop-entry identifier, such as org.example.App.desktop",
                    },
                },
                "required": ["desktop_entry"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_capture",
        title: "Capture a window",
        description: "Return a PNG of one window, stamped with the rectangle it came from and \
                      the sequence number it corresponds to. Its pixels are in the same \
                      coordinates client_pointer takes, and the reply's `content` names which \
                      part of the window they are, so add that origin to a pixel position to \
                      get the point to click. Pass `rect` to return just part of the window: \
                      checking whether a click landed needs a few hundred pixels around it, \
                      not the whole window. Prefer desktop_snapshot for anything you can learn \
                      from structure; capture is for what only pixels can answer. Capturing a \
                      covered or off-screen window is a separate capability and needs a \
                      compositing server.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "area": { "type": "string", "enum": ["content", "frame"] },
                    "rect": {
                        "type": "object",
                        "description": "Part of the window to return, in the coordinates \
                                        client_pointer takes. Clipped to the window; refused \
                                        only if it lies entirely outside.",
                        "properties": {
                            "x": { "type": "integer" },
                            "y": { "type": "integer" },
                            "width": { "type": "integer", "minimum": 1 },
                            "height": { "type": "integer", "minimum": 1 },
                        },
                        "required": ["x", "y", "width", "height"],
                        "additionalProperties": false,
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "output_capture",
        title: "Capture a whole output",
        description: "Return a PNG of an entire display. This is permission to see every \
                      pixel currently on that display, including windows belonging to other \
                      applications, so it is refused outright while any window the user marked \
                      sensitive is visible there.",
        schema: || {
            json!({
                "type": "object",
                "properties": { "output": { "type": "integer", "minimum": 0 } },
                "required": ["output"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_pointer",
        title: "Click inside a window",
        description: "Move the pointer to a point inside a window and optionally click, \
                      double-click, or scroll. Coordinates are relative to the window's own \
                      content area, never to the screen, and are translated against the \
                      window's live position at the moment of injection. They are the same \
                      coordinates as the pixels of a client_capture image, so capture the \
                      window and read the point off the picture. Set ensure_visible to \
                      activate and raise the window first as one operation. If the user is \
                      typing or clicking, the call is refused as interrupted and reports which \
                      steps had already committed. A successful reply means the events were \
                      injected, not that the control under them reacted: capture to confirm.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "x": { "type": "integer", "minimum": 0 },
                    "y": { "type": "integer", "minimum": 0 },
                    "action": {
                        "type": "string",
                        "enum": ["move", "press", "release", "click", "double_click", "scroll"],
                    },
                    "button": {
                        "type": "string",
                        "enum": [
                            "left",
                            "middle",
                            "right",
                            "scroll_up",
                            "scroll_down",
                            "scroll_left",
                            "scroll_right",
                        ],
                    },
                    "ensure_visible": { "type": "boolean" },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client", "x", "y", "action"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_key",
        title: "Send a key to a window",
        description: "Press, release, or tap one named key in a window, optionally with \
                      modifiers held around it. Use this for shortcuts and editing keys; use \
                      client_type for text.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "key": {
                        "type": "string",
                        "description": "X11 keysym name, such as Return, Tab, or a",
                    },
                    "action": { "type": "string", "enum": ["press", "release", "tap"] },
                    "modifiers": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["shift", "control", "alt", "super", "alt_gr"],
                        },
                    },
                    "ensure_visible": { "type": "boolean" },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client", "key", "action"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_type",
        title: "Type text into a window",
        description: "Type text into a window, one character at a time, using the user's \
                      current keyboard layout. Characters the layout cannot produce are \
                      refused rather than approximated. Typing goes wherever the keyboard \
                      focus already is, so click the field first. A successful reply means \
                      the keystrokes were injected, not that anything received them: capture \
                      the window and read the text back before treating it as written.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "text": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "ensure_visible": { "type": "boolean" },
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the window is still what you observed",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "workspace": { "type": "integer", "minimum": 0 },
                            "focused": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["client", "text"],
                "additionalProperties": false,
            })
        },
    },
];

fn main() -> std::process::ExitCode {
    let mut socket: Option<String> = None;
    let mut doctor = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = arguments.next(),
            "doctor" | "--doctor" => doctor = true,
            "--help" | "-h" => {
                let revisions = mcp::supported_versions().join(", ");
                println!(
                    "usage: nobox-agent [--socket PATH]\n       nobox-agent doctor\n\n\
                     Speaks MCP on stdio and the Agent Seat Protocol to a window manager.\n\
                     Supported MCP revisions: {revisions}.\n\n\
                     The socket is taken from --socket, then AGENT_SEAT_SOCKET, then\n\
                     $XDG_RUNTIME_DIR/nobox/agent-seat-<display>.sock. A manager also\n\
                     advertises it in the _AGENT_SEAT root property.\n\n\
                     Register it with a host by giving it this command with no arguments:\n\
                     \x20 claude mcp add nobox -- nobox-agent\n\
                     \x20 codex: [mcp_servers.nobox] command = \"nobox-agent\"\n\n\
                     `nobox-agent doctor` checks the whole path — socket, manager, grant —\n\
                     and prints what a host would be told. Run it when a host reports that\n\
                     this server failed to start."
                );
                return std::process::ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("nobox-agent {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("nobox-agent: unknown argument {other}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let Some(socket) = seat::resolve_socket(socket.as_deref()) else {
        eprintln!(
            "nobox-agent: no agent seat socket; pass --socket or set AGENT_SEAT_SOCKET or \
             XDG_RUNTIME_DIR"
        );
        return std::process::ExitCode::FAILURE;
    };
    if doctor {
        return run_doctor(&socket);
    }
    serve(&socket);
    std::process::ExitCode::SUCCESS
}

/// Checks the whole path from this process to the manager, and says what a
/// host would be told.
///
/// A host that cannot start an MCP server reports very little, and what it
/// does report names the host rather than the cause: the server "failed to
/// start", and the person who installed it correctly is left with nowhere to
/// look. Every stage below can fail on its own, so each is stated on its own,
/// in the order a connection actually goes through them.
fn run_doctor(socket: &Path) -> std::process::ExitCode {
    println!("nobox-agent {}", env!("CARGO_PKG_VERSION"));
    println!("MCP revisions: {}", mcp::supported_versions().join(", "));
    println!("socket: {}", socket.display());
    if !socket.exists() {
        println!("  not present.");
        println!(
            "\nThe seat is off, or this process is looking in the wrong place. Check\n\
             `xprop -root _AGENT_SEAT` in the session you mean: if it is absent, set\n\
             `[agent] enabled = true` and reload nobox. If it names a different path,\n\
             this process has the wrong DISPLAY or XDG_RUNTIME_DIR — pass --socket."
        );
        return std::process::ExitCode::FAILURE;
    }
    println!("  present.");
    println!("connecting (a manager set to ask will raise a consent dialog now)...");
    let seat = match Seat::connect(
        socket,
        "nobox-agent",
        "checking the agent seat",
        &REQUESTED_BUNDLES,
    ) {
        Ok(seat) => seat,
        Err(error) => {
            println!("  failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let welcome = seat.welcome();
    println!(
        "  connected to {} as session {}.",
        welcome.manager, welcome.session
    );
    let atoms: Vec<&str> = welcome
        .granted
        .atoms()
        .into_iter()
        .map(agent_seat_proto::Capability::as_str)
        .collect();
    if atoms.is_empty() {
        println!("grant: nothing.");
        println!(
            "\nThe seat is working and this companion holds no capabilities, so every\n\
             tool will answer `denied`. Either answer the consent dialog, or write a\n\
             grant naming this executable. See the agent harness documentation."
        );
        return std::process::ExitCode::FAILURE;
    }
    println!("grant: {}.", atoms.join(", "));
    println!("tools: {}", TOOLS.len());
    println!("\nEverything a host needs is in place.");
    std::process::ExitCode::SUCCESS
}

/// Runs the stdio loop until the host closes it.
fn serve(socket: &Path) {
    let mut server = Server {
        socket: socket.to_path_buf(),
        seat: None,
        negotiated: None,
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("nobox-agent: stdin failed: {error}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = server.handle(&line) else {
            continue;
        };
        if writeln!(stdout, "{response}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return;
        }
    }
}

struct Server {
    socket: PathBuf,
    seat: Option<Seat>,
    /// Revision agreed by `initialize`, when the host opened that way.
    ///
    /// `None` means nobody has introduced themselves, so requests are read as
    /// the stateless revision and must carry their own `_meta`.
    negotiated: Option<String>,
}

impl Server {
    /// Answers one line of stdio traffic, or nothing for a notification.
    fn handle(&mut self, line: &str) -> Option<String> {
        let incoming = match Incoming::parse(line) {
            Ok(incoming) => incoming,
            Err(error) => return Some(error_response(Value::Null, error).to_string()),
        };
        // Notifications get no reply, including the ones we do not implement.
        // `notifications/initialized` lands here and needs nothing further:
        // the revision was settled by the initialize that preceded it.
        let id = incoming.id.clone()?;
        if incoming.method == "initialize" {
            return Some(self.initialize(id, &incoming.params).to_string());
        }
        if let Err(error) = incoming.check_protocol(self.negotiated.as_deref()) {
            return Some(error_response(id, error).to_string());
        }
        let response = match incoming.method.as_str() {
            "server/discover" => {
                let discovered = self.discover();
                self.reply(id, discovered)
            }
            "tools/list" => self.reply(id, tools_list()),
            "ping" => self.reply(id, json!({})),
            "tools/call" => match self.tools_call(&incoming.params) {
                Ok(result) => self.reply(id, result),
                Err(error) => error_response(id, error),
            },
            other => error_response(
                id,
                error_object(METHOD_NOT_FOUND, &format!("unknown method: {other}"), None),
            ),
        };
        Some(response.to_string())
    }

    /// Answers a classic host's opening handshake.
    ///
    /// This touches nothing but its own arguments, and that is the whole
    /// point. A handshake that reaches for the seat inherits everything the
    /// seat can do: block on a socket, wait on a consent dialog, wait on a
    /// person who is not at their desk. Hosts time the handshake and kill a
    /// server that misses it, so a companion that connects here fails to start
    /// at all — and pops a keyboard-grabbing dialog on someone's screen at
    /// launch, for a session nobody has asked to use yet.
    ///
    /// The seat is therefore reached on the first tool call, where waiting is
    /// something the agent asked for. The guidance sent here is the part that
    /// is true before any of that: how to work the seat. What this particular
    /// session was granted is reported by `server/discover` and by the first
    /// refusal, both of which happen after a connection exists.
    fn initialize(&mut self, id: Value, params: &Map<String, Value>) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let agreed = mcp::negotiate(requested);
        self.negotiated = Some(agreed.clone());
        mcp::plain_result_response(
            id,
            mcp::initialize_result(&agreed, name(), version(), &GUIDANCE.join("\n")),
        )
    }

    /// Stamps a result the way the agreed revision expects.
    fn reply(&self, id: Value, result: Value) -> Value {
        if self.negotiated.is_some() {
            mcp::plain_result_response(id, result)
        } else {
            result_response(id, result, name(), version())
        }
    }

    fn discover(&mut self) -> Value {
        // Reporting the seat's own state here makes the first question a host
        // asks also answer "is the window manager there, and what did it
        // actually grant me".
        json!({
            "supportedVersions": mcp::supported_versions(),
            "capabilities": { "tools": {} },
            "instructions": self.instructions(),
            "ttlMs": CACHE_TTL_MS,
            "cacheScope": "private",
        })
    }

    /// Explains to a model how to work this seat.
    ///
    /// The advice is deliberately about judgement rather than syntax — the
    /// schemas already carry the syntax. What a model cannot infer from a
    /// schema is that structure beats screenshots here, that a refusal can be
    /// the user typing rather than a bug, and that a window it cannot see may
    /// be one the user chose to keep private.
    fn instructions(&mut self) -> String {
        let mut text = GUIDANCE.join("\n");
        text.push_str("\n\nTHIS SESSION\n");
        match self.connect() {
            Ok(seat) => {
                let welcome = seat.welcome();
                let atoms: Vec<&str> = welcome
                    .granted
                    .atoms()
                    .into_iter()
                    .map(agent_seat_proto::Capability::as_str)
                    .collect();
                text.push_str(&format!(
                    "Connected to {} as session {}.\n",
                    welcome.manager, welcome.session
                ));
                if atoms.is_empty() {
                    text.push_str(
                        "Granted: nothing. Every request will be refused until the user grants                          this companion capabilities in the window manager's configuration.                          Tell them that rather than retrying.\n",
                    );
                } else {
                    text.push_str(&format!("Granted: {}.\n", atoms.join(", ")));
                }
                if welcome.scoped {
                    text.push_str(
                        "This grant is scoped to particular applications: windows outside it do                          not appear at all, and that is not a fault.\n",
                    );
                }
                if !welcome.features.is_empty() {
                    text.push_str(&format!(
                        "This desktop can also: {}.\n",
                        welcome
                            .features
                            .iter()
                            .map(feature_summary)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            Err(error) => {
                text.push_str(&format!("Not connected to a window manager: {error}\n"));
                text.push_str(
                    "The desktop may not be running a manager that offers an agent seat, or the                      seat may be turned off in its configuration. Tell the user; do not retry                      blindly.\n",
                );
            }
        }
        text
    }

    fn tools_call(&mut self, params: &Map<String, Value>) -> Result<Value, Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| error_object(INVALID_PARAMS, "tools/call requires a tool name", None))?;
        let empty = Map::new();
        let arguments = params
            .get("arguments")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        if name == "events_poll" {
            return self.poll(arguments);
        }
        let call = build_call(name, arguments)?;
        let seat = self
            .connect()
            .map_err(|error| tool_failure(&format!("the agent seat is unreachable: {error}")))?;
        match seat.call(call) {
            Ok(Outcome::Ok { reply }) => Ok(tool_success(&reply)),
            Ok(Outcome::Error { error }) => Ok(tool_refusal(&error)),
            Err(transport) => {
                // A broken session is not reusable; the next call reconnects.
                self.seat = None;
                Ok(tool_failure(&transport))
            }
        }
    }

    /// Cursor-based event retrieval.
    ///
    /// Statelessness is why this is a poll rather than a push: the client
    /// passes back the sequence it has reached, so nothing depends on this
    /// process having served the earlier calls.
    fn poll(&mut self, arguments: &Map<String, Value>) -> Result<Value, Value> {
        let after = Sequence::new(required_u64(arguments, "after_seq")?);
        let wait = arguments
            .get("wait_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(30_000);
        let seat = self
            .connect()
            .map_err(|error| tool_failure(&format!("the agent seat is unreachable: {error}")))?;
        match seat.poll_events(after, Duration::from_millis(wait)) {
            Ok(events) => {
                let highest = events
                    .iter()
                    .map(|envelope| envelope.sequence.raw())
                    .max()
                    .unwrap_or(after.raw());
                let structured = json!({
                    "events": serde_json::to_value(&events).unwrap_or(Value::Null),
                    "sequence": highest,
                });
                Ok(json!({
                    "content": [{ "type": "text", "text": structured.to_string() }],
                    "structuredContent": structured,
                    "isError": false,
                }))
            }
            Err(transport) => {
                self.seat = None;
                Ok(tool_failure(&transport))
            }
        }
    }

    fn connect(&mut self) -> Result<&mut Seat, String> {
        if self.seat.is_none() {
            let seat = Seat::connect(
                &self.socket,
                "nobox-agent",
                "MCP companion for an agent harness",
                &REQUESTED_BUNDLES,
            )?;
            eprintln!(
                "nobox-agent: connected to {} as session {}",
                seat.welcome().manager,
                seat.welcome().session
            );
            self.seat = Some(seat);
        }
        self.seat.as_mut().ok_or_else(|| "not connected".to_owned())
    }
}

/// Translates an MCP tool call into a seat call.
fn build_call(name: &str, arguments: &Map<String, Value>) -> Result<Call, Value> {
    match name {
        "desktop_snapshot" => Ok(Call::DesktopSnapshot {}),
        "desktop_subscribe" => Ok(Call::SubscribeAndSnapshot {
            kinds: optional_kinds(arguments)?,
        }),
        "client_get" => Ok(Call::ClientGet {
            client: ClientId::new(required_u64(arguments, "client")?),
        }),
        "client_activate" => Ok(Call::ClientActivate {
            client: ClientId::new(required_u64(arguments, "client")?),
            expects: optional_expects(arguments)?,
        }),
        "client_close" => Ok(Call::ClientClose {
            client: ClientId::new(required_u64(arguments, "client")?),
            expects: optional_expects(arguments)?,
        }),
        "client_move_resize" => Ok(Call::ClientMoveResize {
            client: ClientId::new(required_u64(arguments, "client")?),
            geometry: GeometryRequest {
                x: optional_i32(arguments, "x")?,
                y: optional_i32(arguments, "y")?,
                width: optional_u32(arguments, "width")?,
                height: optional_u32(arguments, "height")?,
            },
            expects: optional_expects(arguments)?,
        }),
        "client_set_state" => Ok(Call::ClientSetState {
            client: ClientId::new(required_u64(arguments, "client")?),
            change: StateChange {
                minimized: optional_bool(arguments, "minimized"),
                maximized_horizontal: optional_bool(arguments, "maximized_horizontal"),
                maximized_vertical: optional_bool(arguments, "maximized_vertical"),
                fullscreen: optional_bool(arguments, "fullscreen"),
                shaded: optional_bool(arguments, "shaded"),
                sticky: optional_bool(arguments, "sticky"),
                above: optional_bool(arguments, "above"),
                below: optional_bool(arguments, "below"),
            },
            expects: optional_expects(arguments)?,
        }),
        "client_send_to_workspace" => Ok(Call::ClientSendToWorkspace {
            client: ClientId::new(required_u64(arguments, "client")?),
            workspace: WorkspaceId::new(required_u32(arguments, "workspace")?),
            follow: optional_bool(arguments, "follow").unwrap_or(false),
            expects: optional_expects(arguments)?,
        }),
        "launch" => Ok(Call::Launch {
            desktop_entry: required_string(arguments, "desktop_entry")?,
            uris: Vec::new(),
        }),
        "client_capture" => Ok(Call::ClientCapture {
            client: ClientId::new(required_u64(arguments, "client")?),
            area: optional_enum::<CaptureArea>(arguments, "area")?.unwrap_or_default(),
            rect: optional_rect(arguments)?,
            expects: optional_expects(arguments)?,
        }),
        "output_capture" => Ok(Call::OutputCapture {
            output: OutputId::new(required_u64(arguments, "output")?),
        }),
        "client_pointer" => Ok(Call::ClientPointer {
            client: ClientId::new(required_u64(arguments, "client")?),
            x: optional_i32(arguments, "x")?.unwrap_or(0),
            y: optional_i32(arguments, "y")?.unwrap_or(0),
            action: required_enum(arguments, "action")?,
            button: optional_enum::<PointerButton>(arguments, "button")?,
            ensure_visible: optional_bool(arguments, "ensure_visible").unwrap_or(false),
            expects: optional_expects(arguments)?,
        }),
        "client_key" => Ok(Call::ClientKey {
            client: ClientId::new(required_u64(arguments, "client")?),
            key: required_string(arguments, "key")?,
            action: required_enum::<KeyAction>(arguments, "action")?,
            modifiers: optional_enum_list::<Modifier>(arguments, "modifiers")?,
            ensure_visible: optional_bool(arguments, "ensure_visible").unwrap_or(false),
            expects: optional_expects(arguments)?,
        }),
        "client_type" => Ok(Call::ClientType {
            client: ClientId::new(required_u64(arguments, "client")?),
            text: required_string(arguments, "text")?,
            ensure_visible: optional_bool(arguments, "ensure_visible").unwrap_or(false),
            expects: optional_expects(arguments)?,
        }),
        "workspace_switch" => Ok(Call::WorkspaceSwitch {
            workspace: WorkspaceId::new(required_u32(arguments, "workspace")?),
        }),
        other => Err(error_object(
            INVALID_PARAMS,
            &format!("Unknown tool: {other}"),
            None,
        )),
    }
}

/// Parses an optional list of event kinds, refusing names this build does not
/// know rather than silently widening the stream.
fn optional_kinds(arguments: &Map<String, Value>) -> Result<Vec<EventKind>, Value> {
    let Some(kinds) = arguments.get("kinds") else {
        return Ok(Vec::new());
    };
    let kinds = kinds.as_array().ok_or_else(|| {
        error_object(
            INVALID_PARAMS,
            "kinds must be an array of event names",
            None,
        )
    })?;
    kinds
        .iter()
        .map(|kind| {
            serde_json::from_value::<EventKind>(kind.clone()).map_err(|error| {
                error_object(
                    INVALID_PARAMS,
                    &format!("unknown event kind: {error}"),
                    None,
                )
            })
        })
        .collect()
}

/// Parses the optional freshness block. Unknown fields are refused rather than
/// ignored: a precondition the manager silently drops is worse than none.
fn optional_rect(arguments: &Map<String, Value>) -> Result<Option<Rect>, Value> {
    let Some(rect) = arguments.get("rect") else {
        return Ok(None);
    };
    serde_json::from_value(rect.clone())
        .map(Some)
        .map_err(|error| error_object(INVALID_PARAMS, &format!("unusable rect: {error}"), None))
}

fn optional_expects(arguments: &Map<String, Value>) -> Result<Expects, Value> {
    let Some(expects) = arguments.get("expects") else {
        return Ok(Expects::default());
    };
    serde_json::from_value(expects.clone()).map_err(|error| {
        error_object(
            INVALID_PARAMS,
            &format!("unusable expects block: {error}"),
            None,
        )
    })
}

fn required_string(arguments: &Map<String, Value>, field: &str) -> Result<String, Value> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| error_object(INVALID_PARAMS, &format!("{field} must be a string"), None))
}

/// Parses a named protocol value, refusing anything this build does not know.
fn required_enum<T: serde::de::DeserializeOwned>(
    arguments: &Map<String, Value>,
    field: &str,
) -> Result<T, Value> {
    let value = arguments
        .get(field)
        .ok_or_else(|| error_object(INVALID_PARAMS, &format!("{field} is required"), None))?;
    serde_json::from_value(value.clone())
        .map_err(|error| error_object(INVALID_PARAMS, &format!("unusable {field}: {error}"), None))
}

fn optional_enum<T: serde::de::DeserializeOwned>(
    arguments: &Map<String, Value>,
    field: &str,
) -> Result<Option<T>, Value> {
    if arguments.get(field).is_none() {
        return Ok(None);
    }
    required_enum(arguments, field).map(Some)
}

fn optional_enum_list<T: serde::de::DeserializeOwned>(
    arguments: &Map<String, Value>,
    field: &str,
) -> Result<Vec<T>, Value> {
    let Some(values) = arguments.get(field) else {
        return Ok(Vec::new());
    };
    let values = values
        .as_array()
        .ok_or_else(|| error_object(INVALID_PARAMS, &format!("{field} must be an array"), None))?;
    values
        .iter()
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                error_object(INVALID_PARAMS, &format!("unusable {field}: {error}"), None)
            })
        })
        .collect()
}

fn optional_bool(arguments: &Map<String, Value>, field: &str) -> Option<bool> {
    arguments.get(field).and_then(Value::as_bool)
}

fn optional_i32(arguments: &Map<String, Value>, field: &str) -> Result<Option<i32>, Value> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    value
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| error_object(INVALID_PARAMS, &format!("{field} must fit in i32"), None))
}

fn optional_u32(arguments: &Map<String, Value>, field: &str) -> Result<Option<u32>, Value> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| error_object(INVALID_PARAMS, &format!("{field} must fit in u32"), None))
}

fn required_u32(arguments: &Map<String, Value>, field: &str) -> Result<u32, Value> {
    u32::try_from(required_u64(arguments, field)?)
        .map_err(|_| error_object(INVALID_PARAMS, &format!("{field} must fit in u32"), None))
}

fn required_u64(arguments: &Map<String, Value>, field: &str) -> Result<u64, Value> {
    arguments
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| error_object(INVALID_PARAMS, &format!("{field} must be an integer"), None))
}

fn tools_list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect();
    json!({ "tools": tools, "ttlMs": CACHE_TTL_MS, "cacheScope": "private" })
}

/// A successful call: structured content, plus its serialization as text for
/// clients that do not read structured results.
fn tool_success(reply: &agent_seat_proto::Reply) -> Value {
    let mut structured = serde_json::to_value(reply).unwrap_or(Value::Null);
    // A capture is the one reply whose payload is pixels rather than facts.
    // Serialized as text it is a wall of base64 that a host will truncate and
    // a model cannot look at, which leaves an agent guessing at coordinates it
    // was given a picture of. Hand the bytes over as an image block, and keep
    // the geometry beside it as data.
    if let Some(image) = capture_image(&mut structured) {
        return json!({
            "content": [
                { "type": "image", "data": image, "mimeType": "image/png" },
                { "type": "text", "text": structured.to_string() },
            ],
            "structuredContent": structured,
            "isError": false,
        });
    }
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": false,
    })
}

/// Lifts the base64 payload out of a capture reply, leaving its metadata.
///
/// Taking the data rather than copying it is deliberate: the bytes then travel
/// exactly once, in the block built to carry them, instead of three times in
/// two encodings.
fn capture_image(structured: &mut Value) -> Option<String> {
    let image = structured.get_mut("image")?.as_object_mut()?;
    match image.remove("data")? {
        Value::String(data) => Some(data),
        other => {
            image.insert("data".to_owned(), other);
            None
        }
    }
}

/// A refusal by the manager. This is actionable feedback a model can correct
/// against — a missing capability, a stale precondition — so it is a tool
/// execution error rather than a protocol error.
fn tool_refusal(error: &agent_seat_proto::ProtocolError) -> Value {
    let structured = serde_json::to_value(error).unwrap_or(Value::Null);
    json!({
        "content": [{
            "type": "text",
            "text": format!("{}: {}", error.code.as_str(), error.message),
        }],
        "structuredContent": structured,
        "isError": true,
    })
}

fn tool_failure(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

/// Describes a backend-dependent ability in a model-readable way.
const fn feature_summary(feature: &agent_seat_proto::Feature) -> &'static str {
    match feature {
        agent_seat_proto::Feature::ObscuredCapture => "capture windows that are covered",
        agent_seat_proto::Feature::OutputCapture => "capture a whole display",
        agent_seat_proto::Feature::InputInjection => "inject input",
        agent_seat_proto::Feature::DesktopLaunch => "start installed applications",
    }
}

const fn name() -> &'static str {
    "nobox-agent"
}

const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::{TOOLS, build_call, tool_refusal, tool_success, tools_list};
    use agent_seat_proto::{Call, ErrorCode, ProtocolError};
    use serde_json::{Map, Value, json};

    fn arguments(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn the_instructions_tell_a_model_what_the_schemas_cannot() {
        // Not connected: the guidance is still complete, and says so.
        let mut server = super::Server {
            socket: std::path::PathBuf::from("/nonexistent/agent-seat.sock"),
            seat: None,
            negotiated: None,
        };
        let instructions = server.instructions();
        for topic in [
            "desktop_snapshot",
            "after_seq",
            "generation",
            "stale_state",
            "interrupted",
            "session_frozen",
            "no_such_client",
            "denied",
        ] {
            assert!(instructions.contains(topic), "missing guidance on {topic}");
        }
        assert!(
            instructions.contains("Not connected to a window manager"),
            "an unreachable seat must be reported rather than implied"
        );
        assert!(
            instructions.contains("do not retry"),
            "a model must be told when retrying is the wrong move"
        );
    }

    #[test]
    fn the_tool_list_is_deterministic_and_cacheable() {
        let first = tools_list();
        let second = tools_list();
        assert_eq!(first, second);
        assert!(first["ttlMs"].as_u64().is_some());
        assert_eq!(first["cacheScope"], "private");
        let names: Vec<&str> = first["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            vec![
                "desktop_snapshot",
                "desktop_subscribe",
                "events_poll",
                "client_get",
                "client_activate",
                "client_close",
                "client_move_resize",
                "client_set_state",
                "client_send_to_workspace",
                "workspace_switch",
                "launch",
                "client_capture",
                "output_capture",
                "client_pointer",
                "client_key",
                "client_type",
            ]
        );
    }

    #[test]
    fn every_tool_declares_an_object_schema() {
        for tool in TOOLS {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert!(!tool.description.is_empty(), "{}", tool.name);
        }
    }

    #[test]
    fn event_kind_filters_are_validated_not_guessed() {
        let call = build_call(
            "desktop_subscribe",
            &arguments(json!({ "kinds": ["title_changed", "focus_changed"] })),
        )
        .expect("built");
        let Call::SubscribeAndSnapshot { kinds } = call else {
            panic!("wrong call");
        };
        assert_eq!(kinds.len(), 2);
        let error = build_call(
            "desktop_subscribe",
            &arguments(json!({ "kinds": ["everything"] })),
        )
        .expect_err("rejected");
        assert_eq!(error["code"], super::INVALID_PARAMS);
    }

    #[test]
    fn tool_arguments_become_seat_calls() {
        assert!(matches!(
            build_call("desktop_snapshot", &Map::new()).expect("built"),
            Call::DesktopSnapshot {}
        ));
        let call = build_call("client_get", &arguments(json!({ "client": 7 }))).expect("built");
        assert!(matches!(call, Call::ClientGet { client } if client.raw() == 7));
    }

    #[test]
    fn freshness_blocks_are_parsed_rather_than_ignored() {
        let call = build_call(
            "client_activate",
            &arguments(json!({
                "client": 4,
                "expects": { "generation": 7, "focused": false },
            })),
        )
        .expect("built");
        let Call::ClientActivate { expects, .. } = call else {
            panic!("wrong call");
        };
        assert_eq!(expects.generation.map(|value| value.raw()), Some(7));
        assert_eq!(expects.focused, Some(false));

        // A precondition the manager would silently drop is worse than none.
        let error = build_call(
            "client_activate",
            &arguments(json!({ "client": 4, "expects": { "geometry": {} } })),
        )
        .expect_err("rejected");
        assert_eq!(error["code"], super::INVALID_PARAMS);
    }

    #[test]
    fn state_changes_carry_only_what_was_asked_for() {
        let call = build_call(
            "client_set_state",
            &arguments(json!({ "client": 1, "fullscreen": true })),
        )
        .expect("built");
        let Call::ClientSetState { change, .. } = call else {
            panic!("wrong call");
        };
        assert_eq!(change.fullscreen, Some(true));
        assert_eq!(change.minimized, None);
        assert_eq!(change.sticky, None);
    }

    #[test]
    fn input_is_window_addressed_and_named_rather_than_numbered() {
        let call = build_call(
            "client_pointer",
            &arguments(json!({
                "client": 3,
                "x": 40,
                "y": 12,
                "action": "click",
                "button": "left",
                "ensure_visible": true,
            })),
        )
        .expect("built");
        let Call::ClientPointer {
            x,
            y,
            action,
            button,
            ensure_visible,
            ..
        } = call
        else {
            panic!("wrong call");
        };
        assert_eq!((x, y), (40, 12));
        assert_eq!(action, agent_seat_proto::PointerAction::Click);
        assert_eq!(button, Some(agent_seat_proto::PointerButton::Left));
        assert!(ensure_visible);

        let error = build_call(
            "client_pointer",
            &arguments(json!({ "client": 3, "x": 0, "y": 0, "action": "teleport" })),
        )
        .expect_err("rejected");
        assert_eq!(error["code"], super::INVALID_PARAMS);
    }

    #[test]
    fn there_is_no_way_to_ask_for_global_input() {
        // Every input tool takes a window; the protocol cannot express a
        // screen coordinate, so neither can this server.
        for tool in TOOLS {
            let schema = (tool.schema)();
            if !tool.name.starts_with("client_p")
                && !tool.name.starts_with("client_k")
                && !tool.name.starts_with("client_t")
            {
                continue;
            }
            let required = schema["required"].as_array().expect("required");
            assert!(
                required.iter().any(|field| field == "client"),
                "{} does not require a window",
                tool.name
            );
        }
    }

    #[test]
    fn a_missing_argument_is_invalid_params_not_a_guess() {
        let error = build_call("client_get", &Map::new()).expect_err("rejected");
        assert_eq!(error["code"], super::INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error() {
        let error = build_call("rm_rf", &Map::new()).expect_err("rejected");
        assert_eq!(error["code"], super::INVALID_PARAMS);
        assert!(
            error["message"]
                .as_str()
                .expect("message")
                .contains("Unknown tool")
        );
    }

    #[test]
    fn a_refusal_is_reported_as_a_tool_error_the_model_can_act_on() {
        let result = tool_refusal(&ProtocolError::denied("no grant"));
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["code"], "denied");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .expect("text")
                .starts_with("denied:")
        );
    }

    #[test]
    fn a_hidden_client_refusal_says_nothing_a_probe_could_use() {
        let hidden = tool_refusal(&ProtocolError::no_such_client());
        let absent = tool_refusal(&ProtocolError::new(
            ErrorCode::NoSuchClient,
            "no such client",
        ));
        assert_eq!(hidden, absent);
    }

    #[test]
    fn a_success_carries_both_structured_and_text_content() {
        let reply = agent_seat_proto::Reply::Launched {
            launch: "token".to_owned(),
        };
        let result = tool_success(&reply);
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["reply"], "launched");
        assert!(
            !result["content"][0]["text"]
                .as_str()
                .expect("text")
                .is_empty()
        );
    }

    #[test]
    fn a_capture_hands_over_pixels_as_an_image_block() {
        let reply = agent_seat_proto::Reply::Capture {
            image: agent_seat_proto::CaptureImage {
                format: agent_seat_proto::ImageFormat::Png,
                width: 8,
                height: 4,
                source: agent_seat_proto::Rect::new(10, 20, 8, 4),
                content: Some(agent_seat_proto::Rect::new(0, 0, 8, 4)),
                sequence: agent_seat_proto::Sequence::new(7),
                data: agent_seat_proto::Base64Bytes::from(vec![1, 2, 3, 4]),
            },
        };
        let result = tool_success(&reply);
        assert_eq!(result["content"][0]["type"], "image");
        assert_eq!(result["content"][0]["mimeType"], "image/png");
        let encoded = result["content"][0]["data"].as_str().expect("base64");
        assert!(!encoded.is_empty());

        // The geometry a caller needs to turn pixels into pointer coordinates
        // stays as data, beside the picture.
        assert_eq!(result["structuredContent"]["image"]["width"], 8);
        assert_eq!(result["structuredContent"]["image"]["source"]["x"], 10);

        // And the bytes travel once. Repeating them as text is what made a
        // capture unreadable: hosts truncate the blob and the model goes blind.
        assert!(result["structuredContent"]["image"].get("data").is_none());
        let text = result["content"][1]["text"].as_str().expect("text");
        assert!(!text.contains(encoded));
    }
}
