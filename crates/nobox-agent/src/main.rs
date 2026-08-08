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

use nobox_agent_wire::{
    Bundle, Call, CaptureArea, CaptureGrid, ClientId, EventKind, Expected, ExpectedKind, Expects,
    Generation, GeometryRequest, KeyAction, MAX_ACTION_OBSERVATION_MS, MAX_CAPTURE_GRID_SPACING,
    MAX_SEMANTIC_DEPTH, MAX_SEMANTIC_FILTER_ITEMS, MAX_SEMANTIC_NODES, MAX_SEMANTIC_QUERY_LEN,
    MIN_CAPTURE_GRID_SPACING, Modifier, ObservationCapture, ObservationRequest, Outcome, OutputId,
    PointerButton, ProtocolError, ReceivedKind, Rect, SemanticContinuation, SemanticNodeHandle,
    SemanticNodeId, SemanticQuery, Sequence, StateChange, TreeGeneration, WorkspaceId,
};
use serde_json::{Map, Value, json};

use mcp::{
    INVALID_REQUEST, Incoming, METHOD_NOT_FOUND, error_object, error_response, invalid_params,
    result_response,
};
use seat::Seat;

/// How long a client may cache `tools/list` and `server/discover`.
const CACHE_TTL_MS: u64 = 300_000;

const SEMANTIC_ROLES: &[&str] = &[
    "application",
    "window",
    "dialog",
    "document",
    "heading",
    "paragraph",
    "link",
    "button",
    "check_box",
    "radio_button",
    "combo_box",
    "text",
    "entry",
    "list",
    "list_item",
    "table",
    "cell",
    "image",
    "video",
    "audio",
    "menu",
    "menu_item",
    "tab",
    "tab_list",
    "toolbar",
    "status",
    "slider",
    "spin_button",
    "progress",
    "scroll_bar",
    "separator",
    "tooltip",
    "group",
    "section",
    "form",
    "landmark",
    "unknown",
];

const SEMANTIC_STATES: &[&str] = &[
    "active",
    "busy",
    "checked",
    "collapsed",
    "disabled",
    "editable",
    "expanded",
    "focusable",
    "focused",
    "invalid",
    "modal",
    "multiline",
    "offscreen",
    "pressed",
    "protected",
    "read_only",
    "required",
    "selected",
    "selectable",
    "visible",
];

/// What this companion asks a manager for when it connects.
///
/// It exposes tools over every bundle, so it names every bundle rather than
/// asking narrowly and then failing at the first tool outside the request.
/// This is a request, not a claim: the manager grants what it decides to, and
/// a person answering a consent dialog can refuse any of it.
// Accessibility is requested because the companion advertises a semantic
// observation tool. This remains only a request; the manager is the authority.
const REQUESTED_BUNDLES: [Bundle; 6] = [
    Bundle::Observe,
    Bundle::Accessibility,
    Bundle::Capture,
    Bundle::Input,
    Bundle::Manage,
    Bundle::Launch,
];

/// Static, cross-tool guidance an MCP host may put in front of its model.
///
/// Tool descriptions own tool-specific mechanics. This string instead teaches
/// the routing and safety decisions that span tools, with the complete primary
/// workflow inside the first 512 bytes for hosts that truncate instructions.
const SERVER_INSTRUCTIONS: &str = concat!(
    "A permission-scoped GUI seat: state, pixels, launches, window input, and management. ",
    "Use exact sources for files, ",
    "URLs, APIs, builds, and version control. Start with ",
    "`desktop_snapshot`, or `desktop_subscribe` for multi-step work; apply events in order and ",
    "resnapshot after `resync_required`. Prefer `client_semantic_find`, then semantic tree pages; ",
    "use `client_capture` only ",
    "for pixels, click coordinates, and post-input verification. Use narrow `expects`; ",
    "`generation` includes title changes. ",
    "Input uses content coordinates and reports injection, not delivery. `observe` waits for ",
    "events; add pixels only if needed, targeting a stable parent if a dialog may close. Put all ",
    "multiline text in one `client_type` call; Return is not a line builder. Never bypass ",
    "`denied`, hidden, or ",
    "out-of-scope windows; `no_such_client` conflates gone, hidden, and out of scope. Follow ",
    "structured `retryable`; ignore diagnostic text for recovery. Use `seat_status` only ",
    "to diagnose availability and grants."
);

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
        name: "seat_status",
        title: "Agent seat status",
        description: "Connect to the window manager and report whether its agent seat is \
                      available, which manager answered, and what this companion was granted. \
                      Use this when a desktop tool is unavailable; unlike server discovery, this \
                      may raise the window manager's consent dialog.",
        schema: || json!({ "type": "object", "additionalProperties": false }),
    },
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
        name: "client_semantic_root",
        title: "Window semantic root",
        description: "Return the bounded accessibility root for one window: role, accessible \
                      name, states, content-relative bounds, and child count. This is the first \
                      call when desktop structure identifies the right window but the task \
                      depends on its controls or content. The returned tree and node handles \
                      are observation-scoped; do not invent descendants. If semantics are \
                      unavailable, use client_capture only when pixels can answer the task. \
                      Semantic work is single-flight: run these tools sequentially; concurrent \
                      excess fails closed as semantic_unavailable.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 1 },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_semantic_tree",
        title: "Window semantic tree page",
        description: "Return one bounded breadth-first semantic subtree page. Call \
                      client_semantic_root first. Pass its root handle, or omit root for the \
                      current client root. Follow continuation exactly for later pages; it \
                      retains the original subtree and depth. Handles are valid only for their \
                      tree generation, and stale_tree supplies the current generation. Semantic \
                      work is single-flight: run these tools sequentially; concurrent excess \
                      fails closed as semantic_unavailable.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 1 },
                    "root": {
                        "type": "object",
                        "properties": {
                            "tree": { "type": "integer", "minimum": 1 },
                            "node": { "type": "integer", "minimum": 1 },
                        },
                        "required": ["tree", "node"],
                        "additionalProperties": false,
                    },
                    "continuation": { "type": "integer", "minimum": 1 },
                    "max_nodes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SEMANTIC_NODES,
                        "default": 64,
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_SEMANTIC_DEPTH,
                        "default": 8,
                    },
                },
                "required": ["client"],
                "additionalProperties": false,
            })
        },
    },
    ToolDefinition {
        name: "client_semantic_find",
        title: "Find window semantics",
        description: "Find semantic nodes in deterministic breadth-first order using a bounded \
                      accessible-name substring, role OR-filter, and state AND-filter. Call \
                      client_semantic_root first. Prefer this over downloading tree pages when \
                      the desired control or content can be described. Follow continuation \
                      exactly; it retains the original predicate. Semantic work is \
                      single-flight: run these tools sequentially; concurrent excess fails \
                      closed as semantic_unavailable.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 1 },
                    "query": semantic_query_schema(),
                    "continuation": { "type": "integer", "minimum": 1 },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SEMANTIC_NODES,
                        "default": 16,
                    },
                },
                "required": ["client", "query"],
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
                      compositing server. Pass `grid` when a multimodal model needs coordinate \
                      lines and numeric labels already aligned to client_pointer coordinates. \
                      For a large window, use a 100-pixel grid to choose a coarse cell, then \
                      capture that cell as a smaller rect with a 50-pixel grid before clicking. \
                      Read the baked-in labels and origin; never derive coordinates from a \
                      resized rendering of the image.",
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
                    "grid": {
                        "type": "object",
                        "description": "Overlay high-contrast coordinate lines and numeric \
                                        labels in client_pointer coordinates. The response \
                                        reports the applied spacing and the coordinate at image \
                                        pixel zero, including for cropped captures.",
                        "properties": {
                            "spacing": {
                                "type": "integer",
                                "minimum": MIN_CAPTURE_GRID_SPACING,
                                "maximum": MAX_CAPTURE_GRID_SPACING,
                                "description": "Pixels between coordinate lines",
                            },
                        },
                        "required": ["spacing"],
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
                      window and read the point off the picture. On a large image, choose a \
                      coarse grid cell and recapture it as a smaller rect before clicking; use \
                      the labels and reported origin, never scaled display dimensions. Set ensure_visible to \
                      activate and raise the window first as one operation. If the user is \
                      typing or clicking, the call is refused as interrupted and reports which \
                      steps had already committed. A successful reply means the events were \
                      injected, not that the control under them reacted. Attach `observe` to wait \
                      for a bounded quiet period and receive a correlated event slice plus final \
                      capture in this reply; pixels are evidence, not proof of causation.",
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
                        "default": "left",
                        "description": "Defaults to left for press, release, click, and \
                                        double_click; required for scroll to name its direction",
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
                    "observe": observation_schema(),
                    "expects": {
                        "type": "object",
                        "description": "Refuse unless the named facts still match; use generation \
                                        only when every descriptor change matters",
                        "properties": {
                            "generation": { "type": "integer", "minimum": 0 },
                            "content": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "integer" },
                                    "y": { "type": "integer" },
                                    "width": { "type": "integer", "minimum": 1 },
                                    "height": { "type": "integer", "minimum": 1 },
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
                      client_type for text. Attach `observe` to return after a bounded quiet \
                      period with correlated events and one final capture.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "key": {
                        "type": "string",
                        "description": "X11 keysym name, such as Return, Tab, Page_Down, or a; \
                                        common aliases such as Enter, Esc, PageDown, PageUp, \
                                        Backspace, Space, and ArrowLeft are accepted",
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
                    "observe": observation_schema(),
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
        title: "Write text into a window",
        description: "Write complete, coherent text in one call, including paragraph and line \
                      breaks as newline characters in `text`; never send client_key Return just \
                      to create those line breaks. Text is delivered as paced character strokes \
                      using the user's \
                      current keyboard layout. Characters the layout cannot produce are \
                      refused rather than approximated. Typing goes wherever the keyboard \
                      focus already is, so click the field first; the write is refused or \
                      stopped as stale if its target client does not retain focus. A successful reply means \
                      the keystrokes were injected, not that anything received them: capture \
                      the window and read the text back before treating it as written. Attach \
                      `observe` to perform that bounded wait and final capture in this call.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "client": { "type": "integer", "minimum": 0 },
                    "text": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "ensure_visible": { "type": "boolean" },
                    "observe": observation_schema(),
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

/// A semantic filter with at least one independently compilable branch.
///
/// Repeating the object shape is redundant under JSON Schema 2020-12, but
/// some model tool-schema compilers inspect each `anyOf` branch in isolation
/// and reject a bare `{ "required": [...] }` subschema. Keeping `type` inside
/// the branches also avoids dialects that reject a sibling type on `anyOf`.
fn semantic_query_schema() -> Value {
    let properties = json!({
        "name": {
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_SEMANTIC_QUERY_LEN,
        },
        "roles": {
            "type": "array",
            "maxItems": MAX_SEMANTIC_FILTER_ITEMS,
            "items": { "type": "string", "enum": SEMANTIC_ROLES },
        },
        "states": {
            "type": "array",
            "maxItems": MAX_SEMANTIC_FILTER_ITEMS,
            "items": { "type": "string", "enum": SEMANTIC_STATES },
        },
    });
    let branch = |required: &str| {
        json!({
            "type": "object",
            "properties": properties.clone(),
            "required": [required],
            "additionalProperties": false,
        })
    };
    json!({
        "anyOf": [branch("name"), branch("roles"), branch("states")],
    })
}

/// Shared schema for the bounded post-input observation contract.
fn observation_schema() -> Value {
    json!({
        "type": "object",
        "description": "After injection, wait at least minimum_ms and until no correlated \
                        desktop event arrives for quiet_ms, but never beyond maximum_ms; then \
                        return correlated events and, when requested, one capture. Events are \
                        temporally correlated, not claimed caused.",
        "properties": {
            "capture": {
                "type": "object",
                "properties": {
                    "client": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Client to capture; defaults to the input target. Name a \
                                        stable parent when the input may close a transient dialog.",
                    },
                    "area": { "type": "string", "enum": ["content", "frame"] },
                    "rect": {
                        "type": "object",
                        "properties": {
                            "x": { "type": "integer" },
                            "y": { "type": "integer" },
                            "width": { "type": "integer", "minimum": 1 },
                            "height": { "type": "integer", "minimum": 1 },
                        },
                        "required": ["x", "y", "width", "height"],
                        "additionalProperties": false,
                    },
                    "grid": {
                        "type": "object",
                        "properties": {
                            "spacing": {
                                "type": "integer",
                                "minimum": MIN_CAPTURE_GRID_SPACING,
                                "maximum": MAX_CAPTURE_GRID_SPACING,
                            },
                        },
                        "required": ["spacing"],
                        "additionalProperties": false,
                    },
                },
                "additionalProperties": false,
            },
            "minimum_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_ACTION_OBSERVATION_MS,
            },
            "quiet_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_ACTION_OBSERVATION_MS,
            },
            "maximum_ms": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_ACTION_OBSERVATION_MS,
            },
        },
        "required": ["minimum_ms", "quiet_ms", "maximum_ms"],
        "additionalProperties": false,
    })
}

fn main() -> std::process::ExitCode {
    let mut socket: Option<String> = None;
    let mut doctor = false;
    let mut print_mcp_config = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => {
                let Some(path) = arguments.next() else {
                    eprintln!("nobox-agent: --socket requires a path");
                    return std::process::ExitCode::FAILURE;
                };
                socket = Some(path);
            }
            "doctor" | "--doctor" => doctor = true,
            "--print-mcp-config" => print_mcp_config = true,
            "--help" | "-h" => {
                let revisions = mcp::supported_versions().join(", ");
                println!(
                    "usage: nobox-agent [--socket PATH]\n       nobox-agent doctor\n       \
                     nobox-agent --print-mcp-config\n\n\
                     Speaks MCP on stdio and the Agent Seat Protocol to a window manager.\n\
                     Supported MCP revisions: {revisions}.\n\n\
                     The socket is taken from --socket, then AGENT_SEAT_SOCKET, then a\n\
                     live selection-bound _AGENT_SEAT property on the X11 root. There\n\
                     is no conventional filesystem fallback.\n\n\
                     Register it with a host by giving it this command with no arguments:\n\
                     \x20 claude mcp add nobox -- nobox-agent\n\
                     \x20 codex: [mcp_servers.nobox] command = \"nobox-agent\"\n\
                     \x20        env_vars = [\"DISPLAY\"]\n\n\
                     `--print-mcp-config` prints the generic JSON registration snippet.\n\
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
    if print_mcp_config {
        println!(
            "{}",
            json!({
                "mcpServers": {
                    "nobox": { "command": "nobox-agent" }
                }
            })
        );
        return std::process::ExitCode::SUCCESS;
    }
    if doctor {
        let socket = match seat::resolve_socket(socket.as_deref().map(Path::new)) {
            Ok(Some(socket)) => socket,
            Ok(None) => {
                eprintln!(
                    "nobox-agent: no live agent seat is advertised; pass --socket, set \
                     AGENT_SEAT_SOCKET, or set DISPLAY for X11 discovery"
                );
                return std::process::ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!("nobox-agent: {error}");
                return std::process::ExitCode::FAILURE;
            }
        };
        return run_doctor(&socket);
    }
    serve(socket.as_deref());
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
             `[agent] enabled = true` and reload nobox. If the selection owner and\n\
             root property disagree, fix or stop the stale provider. Otherwise this\n\
             process has the wrong DISPLAY — pass --socket."
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
        .map(nobox_agent_wire::Capability::as_str)
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
fn serve(socket: Option<&str>) {
    let mut server = Server {
        socket: socket.map(PathBuf::from),
        seat: None,
        protocol: ProtocolState::Undecided,
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
    /// Explicit socket override, or the path resolved on the first tool call.
    socket: Option<PathBuf>,
    seat: Option<Seat>,
    protocol: ProtocolState,
}

/// The one MCP dialect selected by the first request on this stdio process.
enum ProtocolState {
    Undecided,
    Stateless,
    Handshake { version: String, initialized: bool },
}

impl Server {
    /// Answers one line of stdio traffic, or nothing for a notification.
    fn handle(&mut self, line: &str) -> Option<String> {
        let incoming = match Incoming::parse(line) {
            Ok(incoming) => incoming,
            Err(failure) => {
                return Some(error_response(failure.id, failure.error).to_string());
            }
        };
        if incoming.id.is_none() {
            self.notification(&incoming);
            return None;
        }
        let id = incoming.id.clone().unwrap_or(Value::Null);
        if incoming.method == "initialize" {
            return Some(self.initialize(id, &incoming.params).to_string());
        }
        if let Err(error) = self.select_protocol(&incoming) {
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

    /// Selects modern MCP on its first metadata-bearing request, or checks the
    /// revision already agreed by a legacy initialize handshake.
    fn select_protocol(&mut self, incoming: &Incoming) -> Result<(), Value> {
        match &self.protocol {
            ProtocolState::Undecided => {
                incoming.check_protocol(None)?;
                self.protocol = ProtocolState::Stateless;
                Ok(())
            }
            ProtocolState::Stateless => incoming.check_protocol(None),
            ProtocolState::Handshake {
                version,
                initialized,
            } => {
                if !initialized && incoming.method != "ping" {
                    return Err(error_object(
                        INVALID_REQUEST,
                        "send notifications/initialized before making requests",
                        None,
                    ));
                }
                incoming.check_protocol(Some(version.as_str()))
            }
        }
    }

    /// Applies lifecycle notifications. Notifications never receive replies.
    fn notification(&mut self, incoming: &Incoming) {
        if incoming.method == "notifications/initialized"
            && let ProtocolState::Handshake { initialized, .. } = &mut self.protocol
        {
            *initialized = true;
        }
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
    /// session was granted is reported by the first tool response or refusal,
    /// both of which happen after a connection exists.
    fn initialize(&mut self, id: Value, params: &Map<String, Value>) -> Value {
        if !matches!(&self.protocol, ProtocolState::Undecided) {
            return error_response(
                id,
                error_object(
                    INVALID_REQUEST,
                    "initialize may only be sent once, before other requests",
                    None,
                ),
            );
        }
        let requested = match mcp::initialize_version(params) {
            Ok(version) => version,
            Err(error) => return error_response(id, error),
        };
        let agreed = mcp::negotiate(Some(requested));
        self.protocol = ProtocolState::Handshake {
            version: agreed.clone(),
            initialized: false,
        };
        mcp::plain_result_response(
            id,
            mcp::initialize_result(&agreed, name(), version(), SERVER_INSTRUCTIONS),
        )
    }

    /// Stamps a result the way the agreed revision expects.
    fn reply(&self, id: Value, result: Value) -> Value {
        match &self.protocol {
            ProtocolState::Handshake { .. } => mcp::plain_result_response(id, result),
            ProtocolState::Undecided | ProtocolState::Stateless => {
                result_response(id, result, name(), version())
            }
        }
    }

    fn discover(&self) -> Value {
        // Discovery is a startup probe. It must not touch the seat: connecting
        // can wait on a consent dialog and a host may time the probe out as a
        // broken MCP server.
        json!({
            "supportedVersions": mcp::supported_versions(),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": name(), "version": version() },
            "instructions": SERVER_INSTRUCTIONS,
            "ttlMs": CACHE_TTL_MS,
            "cacheScope": "private",
        })
    }

    /// Reports what the live seat granted without repeating static MCP guidance.
    fn seat_status_text(&mut self) -> String {
        let mut text = String::new();
        match self.connect() {
            Ok(seat) => {
                let welcome = seat.welcome();
                let atoms: Vec<&str> = welcome
                    .granted
                    .atoms()
                    .into_iter()
                    .map(nobox_agent_wire::Capability::as_str)
                    .collect();
                text.push_str(&format!(
                    "Connected to {} as session {}.\n",
                    welcome.manager, welcome.session
                ));
                if atoms.is_empty() {
                    text.push_str(concat!(
                        "Granted: nothing. Every request will be refused until the user grants ",
                        "this companion capabilities in the window manager's configuration. ",
                        "Tell them that rather than retrying.\n"
                    ));
                } else {
                    text.push_str(&format!("Granted: {}.\n", atoms.join(", ")));
                }
                if welcome.scoped {
                    text.push_str(concat!(
                        "This grant is scoped to particular applications: windows outside it do ",
                        "not appear at all, and that is not a fault.\n"
                    ));
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
                text.push_str(concat!(
                    "The desktop may not be running a manager that offers an agent seat, or the ",
                    "seat may be turned off in its configuration. Tell the user; do not retry ",
                    "blindly.\n"
                ));
            }
        }
        text
    }

    fn tools_call(&mut self, params: &Map<String, Value>) -> Result<Value, Value> {
        let name_value = params.get("name");
        let name = name_value.and_then(Value::as_str).ok_or_else(|| {
            invalid_params(&invalid_value(
                "/name",
                Expected::one_of(TOOLS.iter().map(|tool| tool.name)),
                name_value,
                "tools/call requires a known tool name",
            ))
        })?;
        let definition = TOOLS.iter().find(|tool| tool.name == name).ok_or_else(|| {
            invalid_params(&ProtocolError::invalid_argument(
                "/name",
                Expected::one_of(TOOLS.iter().map(|tool| tool.name)),
                ReceivedKind::String,
                "unknown tool name",
            ))
        })?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(value) => {
                return Err(invalid_params(&ProtocolError::invalid_argument(
                    "/arguments",
                    Expected::kind(ExpectedKind::Object),
                    received_kind(Some(value)),
                    "tools/call arguments must be an object",
                )));
            }
        };
        validate_known_fields(arguments, definition).map_err(|error| invalid_params(&error))?;
        if name == "seat_status" {
            let status = self.seat_status_text();
            return Ok(json!({
                "content": [{ "type": "text", "text": status }],
                "structuredContent": { "status": status },
                "isError": false,
            }));
        }
        if name == "events_poll" {
            return self.poll(arguments).map_err(|error| invalid_params(&error));
        }
        let call = build_call(name, arguments).map_err(|error| invalid_params(&error))?;
        let seat = match self.connect() {
            Ok(seat) => seat,
            Err(error) => {
                return Ok(tool_failure(&format!(
                    "the agent seat is unreachable: {error}"
                )));
            }
        };
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
    fn poll(&mut self, arguments: &Map<String, Value>) -> Result<Value, ProtocolError> {
        let after = Sequence::new(required_u64(arguments, "after_seq")?);
        let wait = optional_u32(arguments, "wait_ms")?.unwrap_or(0);
        if wait > 30_000 {
            return Err(ProtocolError::invalid_argument(
                "/wait_ms",
                Expected::integer(Some(0), Some(30_000)),
                ReceivedKind::Integer,
                "event wait exceeds the supported bound",
            ));
        }
        let seat = match self.connect() {
            Ok(seat) => seat,
            Err(error) => {
                return Ok(tool_failure(&format!(
                    "the agent seat is unreachable: {error}"
                )));
            }
        };
        match seat.poll_events(after, Duration::from_millis(u64::from(wait))) {
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
            let socket = seat::resolve_socket(self.socket.as_deref())?.ok_or_else(|| {
                "no live agent seat is advertised; pass --socket, set AGENT_SEAT_SOCKET, or \
                 set DISPLAY for X11 discovery"
                    .to_owned()
            })?;
            let seat = Seat::connect(
                &socket,
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
fn build_call(name: &str, arguments: &Map<String, Value>) -> Result<Call, ProtocolError> {
    let call = match name {
        "desktop_snapshot" => Ok(Call::DesktopSnapshot {}),
        "desktop_subscribe" => Ok(Call::SubscribeAndSnapshot {
            kinds: optional_kinds(arguments)?,
        }),
        "client_get" => Ok(Call::ClientGet {
            client: ClientId::new(required_u64(arguments, "client")?),
        }),
        "client_semantic_root" => Ok(Call::ClientSemanticRoot {
            client: ClientId::new(required_u64(arguments, "client")?),
        }),
        "client_semantic_tree" => Ok(Call::ClientSemanticTree {
            client: ClientId::new(required_u64(arguments, "client")?),
            root: optional_semantic_handle(arguments)?,
            continuation: optional_positive_u64(arguments, "continuation")?
                .map(SemanticContinuation::new),
            max_nodes: optional_u32(arguments, "max_nodes")?
                .map_or(Ok(64), u16::try_from)
                .map_err(|_| {
                    invalid_value(
                        "/max_nodes",
                        Expected::integer(Some(1), Some(u64::from(MAX_SEMANTIC_NODES))),
                        arguments.get("max_nodes"),
                        "semantic node limit is outside its bounds",
                    )
                })?,
            max_depth: optional_u32(arguments, "max_depth")?
                .map_or(Ok(8), u8::try_from)
                .map_err(|_| {
                    invalid_value(
                        "/max_depth",
                        Expected::integer(Some(0), Some(u64::from(MAX_SEMANTIC_DEPTH))),
                        arguments.get("max_depth"),
                        "semantic depth is outside its bounds",
                    )
                })?,
        }),
        "client_semantic_find" => Ok(Call::ClientSemanticFind {
            client: ClientId::new(required_u64(arguments, "client")?),
            query: required_semantic_query(arguments)?,
            continuation: optional_positive_u64(arguments, "continuation")?
                .map(SemanticContinuation::new),
            max_results: optional_u32(arguments, "max_results")?
                .map_or(Ok(16), u16::try_from)
                .map_err(|_| {
                    invalid_value(
                        "/max_results",
                        Expected::integer(Some(1), Some(u64::from(MAX_SEMANTIC_NODES))),
                        arguments.get("max_results"),
                        "semantic result limit is outside its bounds",
                    )
                })?,
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
                minimized: optional_bool(arguments, "minimized")?,
                maximized_horizontal: optional_bool(arguments, "maximized_horizontal")?,
                maximized_vertical: optional_bool(arguments, "maximized_vertical")?,
                fullscreen: optional_bool(arguments, "fullscreen")?,
                shaded: optional_bool(arguments, "shaded")?,
                sticky: optional_bool(arguments, "sticky")?,
                above: optional_bool(arguments, "above")?,
                below: optional_bool(arguments, "below")?,
            },
            expects: optional_expects(arguments)?,
        }),
        "client_send_to_workspace" => Ok(Call::ClientSendToWorkspace {
            client: ClientId::new(required_u64(arguments, "client")?),
            workspace: WorkspaceId::new(required_u32(arguments, "workspace")?),
            follow: optional_bool(arguments, "follow")?.unwrap_or(false),
            expects: optional_expects(arguments)?,
        }),
        "launch" => Ok(Call::Launch {
            desktop_entry: required_string(arguments, "desktop_entry")?,
            uris: Vec::new(),
        }),
        "client_capture" => Ok(Call::ClientCapture {
            client: ClientId::new(required_u64(arguments, "client")?),
            area: optional_enum::<CaptureArea>(arguments, "area", &["content", "frame"])?
                .unwrap_or_default(),
            rect: optional_rect(arguments)?,
            grid: optional_grid(arguments)?,
            expects: optional_expects(arguments)?,
        }),
        "output_capture" => Ok(Call::OutputCapture {
            output: OutputId::new(required_u64(arguments, "output")?),
        }),
        "client_pointer" => pointer_call(arguments),
        "client_key" => Ok(Call::ClientKey {
            client: ClientId::new(required_u64(arguments, "client")?),
            key: required_string(arguments, "key")?,
            action: required_enum::<KeyAction>(arguments, "action", &["press", "release", "tap"])?,
            modifiers: optional_enum_list::<Modifier>(
                arguments,
                "modifiers",
                &["shift", "control", "alt", "super", "alt_gr"],
            )?,
            ensure_visible: optional_bool(arguments, "ensure_visible")?.unwrap_or(false),
            expects: optional_expects(arguments)?,
            observe: optional_observation(arguments)?,
        }),
        "client_type" => Ok(Call::ClientType {
            client: ClientId::new(required_u64(arguments, "client")?),
            text: required_string(arguments, "text")?,
            ensure_visible: optional_bool(arguments, "ensure_visible")?.unwrap_or(false),
            expects: optional_expects(arguments)?,
            observe: optional_observation(arguments)?,
        }),
        "workspace_switch" => Ok(Call::WorkspaceSwitch {
            workspace: WorkspaceId::new(required_u32(arguments, "workspace")?),
        }),
        _ => Err(ProtocolError::invalid_argument(
            "/name",
            Expected::one_of(TOOLS.iter().map(|tool| tool.name)),
            ReceivedKind::String,
            "unknown tool name",
        )),
    }?;
    call.validate().map_err(|mut error| {
        if let Some(path) = error.path.as_mut() {
            if name == "client_move_resize" {
                *path = path.strip_prefix("/geometry").unwrap_or(path).to_owned();
            } else if name == "client_set_state" {
                *path = path.strip_prefix("/change").unwrap_or(path).to_owned();
            }
        }
        error
    })?;
    Ok(call)
}

/// Builds a pointer call while applying the one ergonomic default its schema
/// promises. Scroll deliberately has no default because its button is the
/// direction; omitting that remains invalid at this MCP boundary.
fn pointer_call(arguments: &Map<String, Value>) -> Result<Call, ProtocolError> {
    let action = required_enum::<nobox_agent_wire::PointerAction>(
        arguments,
        "action",
        &[
            "move",
            "press",
            "release",
            "click",
            "double_click",
            "scroll",
        ],
    )?;
    let button = optional_enum::<PointerButton>(
        arguments,
        "button",
        &[
            "left",
            "middle",
            "right",
            "scroll_up",
            "scroll_down",
            "scroll_left",
            "scroll_right",
        ],
    )?
    .or_else(|| {
        matches!(
            action,
            nobox_agent_wire::PointerAction::Press
                | nobox_agent_wire::PointerAction::Release
                | nobox_agent_wire::PointerAction::Click
                | nobox_agent_wire::PointerAction::DoubleClick
        )
        .then_some(PointerButton::Left)
    });
    let call = Call::ClientPointer {
        client: ClientId::new(required_u64(arguments, "client")?),
        x: optional_i32(arguments, "x")?.unwrap_or(0),
        y: optional_i32(arguments, "y")?.unwrap_or(0),
        action,
        button,
        ensure_visible: optional_bool(arguments, "ensure_visible")?.unwrap_or(false),
        expects: optional_expects(arguments)?,
        observe: optional_observation(arguments)?,
    };
    Ok(call)
}

/// Parses an optional list of event kinds, refusing names this build does not
/// know rather than silently widening the stream.
fn optional_kinds(arguments: &Map<String, Value>) -> Result<Vec<EventKind>, ProtocolError> {
    let Some(kinds) = arguments.get("kinds") else {
        return Ok(Vec::new());
    };
    let kinds = kinds.as_array().ok_or_else(|| {
        invalid_value(
            "/kinds",
            Expected::array(Some(8)),
            Some(kinds),
            "kinds must be an array of event names",
        )
    })?;
    if kinds.len() > 8 {
        return Err(ProtocolError::invalid_argument(
            "/kinds",
            Expected::array(Some(8)),
            ReceivedKind::Array,
            "too many event kinds",
        ));
    }
    const VALUES: &[&str] = &[
        "client_mapped",
        "client_closed",
        "title_changed",
        "focus_changed",
        "state_changed",
        "geometry_changed",
        "workspace_switched",
        "human_activity",
    ];
    kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            serde_json::from_value::<EventKind>(kind.clone()).map_err(|_| {
                invalid_value(
                    &format!("/kinds/{index}"),
                    Expected::one_of(VALUES.iter().copied()),
                    Some(kind),
                    "unknown event kind",
                )
            })
        })
        .collect()
}

/// Parses the optional freshness block. Unknown fields are refused rather than
/// ignored: a precondition the manager silently drops is worse than none.
fn optional_rect(arguments: &Map<String, Value>) -> Result<Option<Rect>, ProtocolError> {
    let Some(rect) = arguments.get("rect") else {
        return Ok(None);
    };
    parse_rect(rect, "/rect", true).map(Some)
}

fn required_semantic_query(arguments: &Map<String, Value>) -> Result<SemanticQuery, ProtocolError> {
    let value = arguments.get("query").ok_or_else(|| {
        invalid_value(
            "/query",
            Expected::object_with_any(["name", "roles", "states"]),
            None,
            "semantic query is required",
        )
    })?;
    let query = required_object(value, "/query")?;
    validate_object_fields(query, "/query", &["name", "roles", "states"])?;
    let name = match query.get("name") {
        None => None,
        Some(value) => Some(value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
            invalid_value(
                "/query/name",
                Expected::string(Some(1), Some(MAX_SEMANTIC_QUERY_LEN)),
                Some(value),
                "semantic name query must be a string",
            )
        })?),
    };
    Ok(SemanticQuery {
        name,
        roles: enum_list_at(query, "roles", "/query/roles", SEMANTIC_ROLES)?,
        states: enum_list_at(query, "states", "/query/states", SEMANTIC_STATES)?,
    })
}

fn optional_grid(arguments: &Map<String, Value>) -> Result<Option<CaptureGrid>, ProtocolError> {
    let Some(grid) = arguments.get("grid") else {
        return Ok(None);
    };
    parse_grid(grid, "/grid").map(Some)
}

fn parse_grid(grid: &Value, path: &str) -> Result<CaptureGrid, ProtocolError> {
    let object = required_object(grid, path)?;
    validate_object_fields(object, path, &["spacing"])?;
    let spacing = required_u32_at(object, "spacing", &format!("{path}/spacing"))?;
    if !(MIN_CAPTURE_GRID_SPACING..=MAX_CAPTURE_GRID_SPACING).contains(&spacing) {
        return Err(ProtocolError::invalid_argument(
            format!("{path}/spacing"),
            Expected::integer(
                Some(i64::from(MIN_CAPTURE_GRID_SPACING)),
                Some(u64::from(MAX_CAPTURE_GRID_SPACING)),
            ),
            ReceivedKind::Integer,
            "capture grid spacing is outside its bounds",
        ));
    }
    Ok(CaptureGrid::new(spacing))
}

/// Parses the optional action-and-observation block shared by input tools.
fn optional_observation(
    arguments: &Map<String, Value>,
) -> Result<Option<ObservationRequest>, ProtocolError> {
    let Some(observe) = arguments.get("observe") else {
        return Ok(None);
    };
    let object = required_object(observe, "/observe")?;
    validate_object_fields(
        object,
        "/observe",
        &["capture", "minimum_ms", "quiet_ms", "maximum_ms"],
    )?;
    let capture = object
        .get("capture")
        .map(|capture_value| {
            let capture = required_object(capture_value, "/observe/capture")?;
            validate_object_fields(
                capture,
                "/observe/capture",
                &["client", "area", "rect", "grid"],
            )?;
            let client = capture
                .get("client")
                .map(|_| {
                    required_u64_at(capture, "client", "/observe/capture/client").map(ClientId::new)
                })
                .transpose()?;
            let area = match capture.get("area") {
                None => CaptureArea::default(),
                Some(value) => serde_json::from_value(value.clone()).map_err(|_| {
                    invalid_value(
                        "/observe/capture/area",
                        Expected::one_of(["content", "frame"]),
                        Some(value),
                        "capture area is not accepted",
                    )
                })?,
            };
            let rect = capture
                .get("rect")
                .map(|value| parse_rect(value, "/observe/capture/rect", true))
                .transpose()?;
            let grid = capture
                .get("grid")
                .map(|value| parse_grid(value, "/observe/capture/grid"))
                .transpose()?;
            Ok(ObservationCapture {
                client,
                area,
                rect,
                grid,
            })
        })
        .transpose()?;
    Ok(Some(ObservationRequest {
        capture,
        minimum_ms: required_u32_at(object, "minimum_ms", "/observe/minimum_ms")?,
        quiet_ms: required_u32_at(object, "quiet_ms", "/observe/quiet_ms")?,
        maximum_ms: required_u32_at(object, "maximum_ms", "/observe/maximum_ms")?,
    }))
}

fn optional_expects(arguments: &Map<String, Value>) -> Result<Expects, ProtocolError> {
    let Some(expects) = arguments.get("expects") else {
        return Ok(Expects::default());
    };
    let object = required_object(expects, "/expects")?;
    validate_object_fields(
        object,
        "/expects",
        &["generation", "content", "workspace", "focused"],
    )?;
    Ok(Expects {
        generation: optional_u64_at(object, "generation", "/expects/generation")?
            .map(Generation::new),
        content: object
            .get("content")
            .map(|content| parse_rect(content, "/expects/content", false))
            .transpose()?,
        workspace: optional_u32_at(object, "workspace", "/expects/workspace")?
            .map(WorkspaceId::new),
        focused: optional_bool_at(object, "focused", "/expects/focused")?,
    })
}

fn required_string(arguments: &Map<String, Value>, field: &str) -> Result<String, ProtocolError> {
    let value = arguments.get(field);
    value
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            invalid_value(
                &json_pointer(field),
                Expected::kind(ExpectedKind::String),
                value,
                "field must be a string",
            )
        })
}

/// Parses a named protocol value, refusing anything this build does not know.
fn required_enum<T: serde::de::DeserializeOwned>(
    arguments: &Map<String, Value>,
    field: &str,
    values: &[&str],
) -> Result<T, ProtocolError> {
    let value = arguments.get(field);
    let value = value.ok_or_else(|| {
        invalid_value(
            &json_pointer(field),
            Expected::one_of(values.iter().copied()),
            None,
            "enum field is required",
        )
    })?;
    serde_json::from_value(value.clone()).map_err(|_| {
        invalid_value(
            &json_pointer(field),
            Expected::one_of(values.iter().copied()),
            Some(value),
            "field is not one of the accepted values",
        )
    })
}

fn optional_enum<T: serde::de::DeserializeOwned>(
    arguments: &Map<String, Value>,
    field: &str,
    values: &[&str],
) -> Result<Option<T>, ProtocolError> {
    if arguments.get(field).is_none() {
        return Ok(None);
    }
    required_enum(arguments, field, values).map(Some)
}

fn optional_enum_list<T: serde::de::DeserializeOwned>(
    arguments: &Map<String, Value>,
    field: &str,
    accepted: &[&str],
) -> Result<Vec<T>, ProtocolError> {
    let Some(values) = arguments.get(field) else {
        return Ok(Vec::new());
    };
    let path = json_pointer(field);
    let values = values.as_array().ok_or_else(|| {
        invalid_value(
            &path,
            Expected::array(None),
            Some(values),
            "field must be an array",
        )
    })?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            serde_json::from_value(value.clone()).map_err(|_| {
                invalid_value(
                    &format!("{path}/{index}"),
                    Expected::one_of(accepted.iter().copied()),
                    Some(value),
                    "array item is not one of the accepted values",
                )
            })
        })
        .collect()
}

fn enum_list_at<T: serde::de::DeserializeOwned>(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
    accepted: &[&str],
) -> Result<Vec<T>, ProtocolError> {
    let Some(values) = object.get(field) else {
        return Ok(Vec::new());
    };
    let values = values.as_array().ok_or_else(|| {
        invalid_value(
            path,
            Expected::array(Some(MAX_SEMANTIC_FILTER_ITEMS)),
            Some(values),
            "semantic filter must be an array",
        )
    })?;
    if values.len() > MAX_SEMANTIC_FILTER_ITEMS {
        return Err(ProtocolError::invalid_argument(
            path,
            Expected::array(Some(MAX_SEMANTIC_FILTER_ITEMS)),
            ReceivedKind::Array,
            "too many semantic filter items",
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            serde_json::from_value(value.clone()).map_err(|_| {
                invalid_value(
                    &format!("{path}/{index}"),
                    Expected::one_of(accepted.iter().copied()),
                    Some(value),
                    "semantic filter item is not accepted",
                )
            })
        })
        .collect()
}

fn optional_bool(
    arguments: &Map<String, Value>,
    field: &str,
) -> Result<Option<bool>, ProtocolError> {
    optional_bool_at(arguments, field, &json_pointer(field))
}

fn optional_i32(arguments: &Map<String, Value>, field: &str) -> Result<Option<i32>, ProtocolError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    value
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| {
            invalid_value(
                &json_pointer(field),
                Expected::integer(Some(i64::from(i32::MIN)), Some(u64::from(i32::MAX as u32))),
                Some(value),
                "field must fit in i32",
            )
        })
}

fn optional_u32(arguments: &Map<String, Value>, field: &str) -> Result<Option<u32>, ProtocolError> {
    optional_u32_at(arguments, field, &json_pointer(field))
}

fn required_u32(arguments: &Map<String, Value>, field: &str) -> Result<u32, ProtocolError> {
    required_u32_at(arguments, field, &json_pointer(field))
}

fn required_u64(arguments: &Map<String, Value>, field: &str) -> Result<u64, ProtocolError> {
    required_u64_at(arguments, field, &json_pointer(field))
}

fn received_kind(value: Option<&Value>) -> ReceivedKind {
    match value {
        None => ReceivedKind::Missing,
        Some(Value::Null) => ReceivedKind::Null,
        Some(Value::Bool(_)) => ReceivedKind::Boolean,
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => ReceivedKind::Integer,
        Some(Value::Number(_)) => ReceivedKind::Number,
        Some(Value::String(_)) => ReceivedKind::String,
        Some(Value::Array(_)) => ReceivedKind::Array,
        Some(Value::Object(_)) => ReceivedKind::Object,
    }
}

fn invalid_value(
    path: &str,
    expected: Expected,
    value: Option<&Value>,
    message: &str,
) -> ProtocolError {
    ProtocolError::invalid_argument(path, expected, received_kind(value), message)
}

fn json_pointer(field: &str) -> String {
    format!("/{}", field.replace('~', "~0").replace('/', "~1"))
}

fn required_object<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a Map<String, Value>, ProtocolError> {
    value.as_object().ok_or_else(|| {
        invalid_value(
            path,
            Expected::kind(ExpectedKind::Object),
            Some(value),
            "field must be an object",
        )
    })
}

fn validate_known_fields(
    arguments: &Map<String, Value>,
    definition: &ToolDefinition,
) -> Result<(), ProtocolError> {
    let schema = (definition.schema)();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    validate_schema_fields(arguments, properties, "")
}

fn validate_schema_fields(
    object: &Map<String, Value>,
    properties: &Map<String, Value>,
    prefix: &str,
) -> Result<(), ProtocolError> {
    for (field, value) in object {
        let path = format!("{prefix}{}", json_pointer(field));
        let Some(field_schema) = properties.get(field) else {
            return Err(invalid_value(
                &path,
                Expected::kind(ExpectedKind::Absent),
                Some(value),
                "field is not accepted by this tool",
            ));
        };
        if let (Some(nested), Some(nested_properties)) = (
            value.as_object(),
            field_schema.get("properties").and_then(Value::as_object),
        ) {
            validate_schema_fields(nested, nested_properties, &path)?;
        }
    }
    Ok(())
}

fn validate_object_fields(
    object: &Map<String, Value>,
    prefix: &str,
    accepted: &[&str],
) -> Result<(), ProtocolError> {
    for (field, value) in object {
        if !accepted.contains(&field.as_str()) {
            return Err(invalid_value(
                &format!("{prefix}{}", json_pointer(field)),
                Expected::kind(ExpectedKind::Absent),
                Some(value),
                "field is not accepted in this object",
            ));
        }
    }
    Ok(())
}

fn optional_bool_at(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Option<bool>, ProtocolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| {
        invalid_value(
            path,
            Expected::kind(ExpectedKind::Boolean),
            Some(value),
            "field must be a boolean",
        )
    })
}

fn optional_u64_at(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Option<u64>, ProtocolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value.as_u64().map(Some).ok_or_else(|| {
        invalid_value(
            path,
            Expected::integer(Some(0), Some(u64::MAX)),
            Some(value),
            "field must be a non-negative integer",
        )
    })
}

fn optional_positive_u64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u64>, ProtocolError> {
    let path = json_pointer(field);
    let value = optional_u64_at(object, field, &path)?;
    if value == Some(0) {
        return Err(invalid_value(
            &path,
            Expected::integer(Some(1), Some(u64::MAX)),
            object.get(field),
            "field must be a positive integer",
        ));
    }
    Ok(value)
}

fn optional_semantic_handle(
    arguments: &Map<String, Value>,
) -> Result<Option<SemanticNodeHandle>, ProtocolError> {
    let Some(value) = arguments.get("root") else {
        return Ok(None);
    };
    let object = required_object(value, "/root")?;
    validate_object_fields(object, "/root", &["tree", "node"])?;
    let tree = required_u64_at(object, "tree", "/root/tree")?;
    let node = required_u64_at(object, "node", "/root/node")?;
    if tree == 0 || node == 0 {
        let (path, received) = if tree == 0 {
            ("/root/tree", object.get("tree"))
        } else {
            ("/root/node", object.get("node"))
        };
        return Err(invalid_value(
            path,
            Expected::integer(Some(1), Some(u64::MAX)),
            received,
            "semantic handle fields must be positive",
        ));
    }
    Ok(Some(SemanticNodeHandle {
        tree: TreeGeneration::new(tree),
        node: SemanticNodeId::new(node),
    }))
}

fn required_u64_at(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<u64, ProtocolError> {
    optional_u64_at(object, field, path)?.ok_or_else(|| {
        invalid_value(
            path,
            Expected::integer(Some(0), Some(u64::MAX)),
            None,
            "integer field is required",
        )
    })
}

fn optional_u32_at(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Option<u32>, ProtocolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| {
            invalid_value(
                path,
                Expected::integer(Some(0), Some(u64::from(u32::MAX))),
                Some(value),
                "field must fit in u32",
            )
        })
}

fn required_u32_at(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<u32, ProtocolError> {
    optional_u32_at(object, field, path)?.ok_or_else(|| {
        invalid_value(
            path,
            Expected::integer(Some(0), Some(u64::from(u32::MAX))),
            None,
            "integer field is required",
        )
    })
}

fn required_i32_at(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<i32, ProtocolError> {
    let value = object.get(field);
    value
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            invalid_value(
                path,
                Expected::integer(Some(i64::from(i32::MIN)), Some(u64::from(i32::MAX as u32))),
                value,
                "field must fit in i32",
            )
        })
}

fn parse_rect(value: &Value, path: &str, positive_extent: bool) -> Result<Rect, ProtocolError> {
    let object = required_object(value, path)?;
    validate_object_fields(object, path, &["x", "y", "width", "height"])?;
    let width_path = format!("{path}/width");
    let height_path = format!("{path}/height");
    let width = required_u32_at(object, "width", &width_path)?;
    let height = required_u32_at(object, "height", &height_path)?;
    if positive_extent && width == 0 {
        return Err(ProtocolError::invalid_argument(
            width_path,
            Expected::integer(Some(1), Some(u64::from(u32::MAX))),
            ReceivedKind::Integer,
            "rectangle width must be positive",
        ));
    }
    if positive_extent && height == 0 {
        return Err(ProtocolError::invalid_argument(
            height_path,
            Expected::integer(Some(1), Some(u64::from(u32::MAX))),
            ReceivedKind::Integer,
            "rectangle height must be positive",
        ));
    }
    Ok(Rect::new(
        required_i32_at(object, "x", &format!("{path}/x"))?,
        required_i32_at(object, "y", &format!("{path}/y"))?,
        width,
        height,
    ))
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
fn tool_success(reply: &nobox_agent_wire::Reply) -> Value {
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
    if let Some(image) = structured.get_mut("image").and_then(Value::as_object_mut) {
        return take_image_data(image);
    }
    let samples = structured
        .get_mut("observation")?
        .get_mut("samples")?
        .as_array_mut()?;
    samples.iter_mut().find_map(|sample| {
        sample
            .get_mut("image")
            .and_then(Value::as_object_mut)
            .and_then(take_image_data)
    })
}

fn take_image_data(image: &mut Map<String, Value>) -> Option<String> {
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
fn tool_refusal(error: &nobox_agent_wire::ProtocolError) -> Value {
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
const fn feature_summary(feature: &nobox_agent_wire::Feature) -> &'static str {
    match feature {
        nobox_agent_wire::Feature::ObscuredCapture => "capture windows that are covered",
        nobox_agent_wire::Feature::OutputCapture => "capture a whole display",
        nobox_agent_wire::Feature::InputInjection => "inject input",
        nobox_agent_wire::Feature::DesktopLaunch => "start installed applications",
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
    use super::{
        REQUESTED_BUNDLES, SEMANTIC_ROLES, SEMANTIC_STATES, SERVER_INSTRUCTIONS, TOOLS, build_call,
        tool_refusal, tool_success, tools_list,
    };
    use nobox_agent_wire::{
        Bundle, Call, ClientId, ErrorCode, ExpectedKind, MAX_SEMANTIC_DEPTH, MAX_SEMANTIC_NODES,
        MAX_SEMANTIC_QUERY_LEN, ProtocolError, ReceivedKind, SemanticNodeHandle, SemanticQuery,
        SemanticRole, SemanticState,
    };
    use serde_json::{Map, Value, json};

    fn arguments(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    fn disconnected_server() -> super::Server {
        super::Server {
            socket: Some(std::path::PathBuf::from("/nonexistent/agent-seat.sock")),
            seat: None,
            protocol: super::ProtocolState::Undecided,
        }
    }

    #[test]
    fn server_instructions_are_compact_and_front_loaded() {
        assert!(
            SERVER_INSTRUCTIONS.len() <= 1_000,
            "server instructions used {} bytes",
            SERVER_INSTRUCTIONS.len()
        );
        let prefix = SERVER_INSTRUCTIONS
            .get(..512)
            .expect("the first 512 instruction bytes must be complete UTF-8");
        for topic in [
            "permission-scoped",
            "desktop_snapshot",
            "desktop_subscribe",
            "resync_required",
            "client_capture",
            "verification",
        ] {
            assert!(prefix.contains(topic), "front matter is missing {topic}");
        }
        for topic in [
            "expects",
            "no_such_client",
            "denied",
            "retryable",
            "diagnostic text",
            "seat_status",
        ] {
            assert!(
                SERVER_INSTRUCTIONS.contains(topic),
                "instructions are missing {topic}"
            );
        }
    }

    #[test]
    fn both_lifecycles_publish_identical_static_instructions() {
        let discovery = disconnected_server().discover();
        let params = json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1" },
        });
        let initialized = disconnected_server()
            .initialize(json!(1), params.as_object().expect("initialize params"));

        assert_eq!(discovery["instructions"], SERVER_INSTRUCTIONS);
        assert_eq!(
            initialized["result"]["instructions"],
            discovery["instructions"]
        );
    }

    #[test]
    fn seat_status_reports_only_live_session_information() {
        let status = disconnected_server().seat_status_text();
        assert!(status.contains("Not connected to a window manager"));
        assert!(status.contains("do not retry"));
        assert!(!status.contains("desktop_snapshot"));
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
                "seat_status",
                "desktop_snapshot",
                "desktop_subscribe",
                "events_poll",
                "client_get",
                "client_semantic_root",
                "client_semantic_tree",
                "client_semantic_find",
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
    fn advertised_semantics_request_the_independent_accessibility_bundle() {
        assert_eq!(
            REQUESTED_BUNDLES,
            [
                Bundle::Observe,
                Bundle::Accessibility,
                Bundle::Capture,
                Bundle::Input,
                Bundle::Manage,
                Bundle::Launch,
            ]
        );
        assert!(TOOLS.iter().any(|tool| tool.name == "client_semantic_root"));
    }

    #[test]
    fn lossy_tool_catalog_retains_the_core_workflow() {
        let listing = tools_list();
        let tools = listing["tools"].as_array().expect("tools");
        let description = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .and_then(|tool| tool["description"].as_str())
                .expect("tool description")
        };

        assert!(description("desktop_snapshot").contains("first call"));
        assert!(description("desktop_subscribe").contains("event stream"));
        assert!(description("client_semantic_root").contains("first call"));
        assert!(description("client_semantic_tree").contains("breadth-first"));
        assert!(description("client_semantic_find").contains("Prefer this"));
        for name in [
            "client_semantic_root",
            "client_semantic_tree",
            "client_semantic_find",
        ] {
            assert!(description(name).contains("sequentially"), "{name}");
            assert!(description(name).contains("semantic_unavailable"), "{name}");
        }
        assert!(description("client_capture").contains("only pixels"));
        assert!(description("client_capture").contains("coarse cell"));
        assert!(description("client_pointer").contains("capture the window"));
        assert!(description("client_pointer").contains("never scaled display dimensions"));
        assert!(description("client_type").contains("capture the window"));
        assert!(description("client_type").contains("never send client_key Return"));
        assert!(description("client_type").contains("paced character strokes"));
        assert!(SERVER_INSTRUCTIONS.contains("one `client_type` call"));
        assert!(description("seat_status").contains("desktop tool is unavailable"));
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
    fn every_required_schema_branch_is_self_contained() {
        fn check(value: &Value, tool: &str) {
            match value {
                Value::Array(values) => {
                    for value in values {
                        check(value, tool);
                    }
                }
                Value::Object(object) => {
                    if let Some(required) = object.get("required").and_then(Value::as_array) {
                        assert_eq!(object.get("type"), Some(&json!("object")), "{tool}");
                        let properties = object
                            .get("properties")
                            .and_then(Value::as_object)
                            .unwrap_or_else(|| panic!("{tool} has required without properties"));
                        for field in required {
                            let field = field.as_str().expect("required field name");
                            assert!(
                                properties.contains_key(field),
                                "{tool} requires undeclared field {field}"
                            );
                        }
                    }
                    for value in object.values() {
                        check(value, tool);
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            }
        }

        for tool in TOOLS {
            check(&(tool.schema)(), tool.name);
        }
    }

    #[test]
    fn client_capture_grid_schema_and_translation_are_bounded() {
        let listing = tools_list();
        let capture = listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "client_capture")
            .expect("client_capture");
        let spacing = &capture["inputSchema"]["properties"]["grid"]["properties"]["spacing"];
        assert_eq!(spacing["minimum"], 50);
        assert_eq!(spacing["maximum"], 512);

        let call = build_call(
            "client_capture",
            &arguments(json!({ "client": 7, "grid": { "spacing": 100 } })),
        )
        .expect("built");
        assert!(matches!(
            call,
            Call::ClientCapture {
                grid: Some(nobox_agent_wire::CaptureGrid { spacing: 100 }),
                ..
            }
        ));
        assert!(
            build_call(
                "client_capture",
                &arguments(json!({
                    "client": 7,
                    "grid": { "spacing": 100, "labels": true },
                })),
            )
            .is_err(),
            "unknown grid fields are refused rather than guessed"
        );
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
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.path.as_deref(), Some("/kinds/0"));
        assert_eq!(error.expected.expect("expected").kind, ExpectedKind::Enum);
    }

    #[test]
    fn tool_arguments_become_seat_calls() {
        assert!(matches!(
            build_call("desktop_snapshot", &Map::new()).expect("built"),
            Call::DesktopSnapshot {}
        ));
        let call = build_call("client_get", &arguments(json!({ "client": 7 }))).expect("built");
        assert!(matches!(call, Call::ClientGet { client } if client.raw() == 7));
        let call =
            build_call("client_semantic_root", &arguments(json!({ "client": 9 }))).expect("built");
        assert!(matches!(
            call,
            Call::ClientSemanticRoot { client } if client.raw() == 9
        ));
        let call = build_call(
            "client_semantic_tree",
            &arguments(json!({
                "client": 9,
                "root": { "tree": 3, "node": 7 },
                "max_nodes": 12,
                "max_depth": 4,
            })),
        )
        .expect("built");
        assert!(matches!(
            call,
            Call::ClientSemanticTree {
                root: Some(SemanticNodeHandle { tree, node }),
                continuation: None,
                max_nodes: 12,
                max_depth: 4,
                ..
            } if tree.raw() == 3 && node.raw() == 7
        ));
        let call = build_call(
            "client_semantic_find",
            &arguments(json!({
                "client": 9,
                "query": {
                    "name": "play",
                    "roles": ["button"],
                    "states": ["visible"],
                },
                "max_results": 7,
            })),
        )
        .expect("built");
        assert!(matches!(
            call,
            Call::ClientSemanticFind {
                query: SemanticQuery { name: Some(name), roles, states },
                continuation: None,
                max_results: 7,
                ..
            } if name == "play"
                && roles == [SemanticRole::Button]
                && states == [SemanticState::Visible]
        ));
    }

    #[test]
    fn semantic_root_schema_is_minimal_and_requires_a_real_client() {
        let listing = tools_list();
        let semantic = listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "client_semantic_root")
            .expect("client_semantic_root");
        assert_eq!(
            semantic["inputSchema"]["properties"]["client"]["minimum"],
            1
        );
        assert_eq!(semantic["inputSchema"]["required"], json!(["client"]));
        assert_eq!(semantic["inputSchema"]["additionalProperties"], false);
    }

    #[test]
    fn semantic_tree_schema_and_translation_are_bounded() {
        let listing = tools_list();
        let semantic = listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "client_semantic_tree")
            .expect("client_semantic_tree");
        let properties = &semantic["inputSchema"]["properties"];
        assert_eq!(properties["max_nodes"]["maximum"], MAX_SEMANTIC_NODES);
        assert_eq!(properties["max_depth"]["maximum"], MAX_SEMANTIC_DEPTH);
        assert_eq!(properties["root"]["additionalProperties"], false);

        let continued = build_call(
            "client_semantic_tree",
            &arguments(json!({ "client": 9, "continuation": 5 })),
        )
        .expect("continued");
        assert!(matches!(
            continued,
            Call::ClientSemanticTree {
                root: None,
                continuation: Some(value),
                max_nodes: 64,
                max_depth: 8,
                ..
            } if value.raw() == 5
        ));
        assert!(
            build_call(
                "client_semantic_tree",
                &arguments(json!({ "client": 9, "continuation": 0 })),
            )
            .is_err()
        );
    }

    #[test]
    fn semantic_find_schema_and_translation_are_constrained() {
        let listing = tools_list();
        let semantic = listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "client_semantic_find")
            .expect("client_semantic_find");
        let properties = &semantic["inputSchema"]["properties"];
        assert_eq!(properties["max_results"]["maximum"], MAX_SEMANTIC_NODES);
        let branches = properties["query"]["anyOf"].as_array().expect("anyOf");
        assert_eq!(branches.len(), 3);
        assert!(properties["query"].get("type").is_none());
        let query_properties = &branches[0]["properties"];
        assert_eq!(
            query_properties["name"]["maxLength"],
            MAX_SEMANTIC_QUERY_LEN
        );
        assert_eq!(
            query_properties["roles"]["items"]["enum"],
            json!(SEMANTIC_ROLES)
        );
        assert_eq!(
            query_properties["states"]["items"]["enum"],
            json!(SEMANTIC_STATES)
        );
        let role_names = SemanticRole::ALL.map(|role| {
            serde_json::to_value(role)
                .expect("serialize role")
                .as_str()
                .expect("role string")
                .to_owned()
        });
        let state_names = SemanticState::ALL.map(|state| {
            serde_json::to_value(state)
                .expect("serialize state")
                .as_str()
                .expect("state string")
                .to_owned()
        });
        assert_eq!(role_names.as_slice(), SEMANTIC_ROLES);
        assert_eq!(state_names.as_slice(), SEMANTIC_STATES);

        let continued = build_call(
            "client_semantic_find",
            &arguments(json!({
                "client": 9,
                "query": { "roles": ["button"] },
                "continuation": 5,
            })),
        )
        .expect("continued");
        assert!(matches!(
            continued,
            Call::ClientSemanticFind {
                continuation: Some(value),
                max_results: 16,
                ..
            } if value.raw() == 5
        ));
        for invalid in [
            json!({ "client": 9, "query": {} }),
            json!({ "client": 9, "query": { "roles": ["hyperlink"] } }),
            json!({ "client": 9, "query": { "states": "visible" } }),
            json!({ "client": 9, "query": { "name": "play", "secret": true } }),
            json!({ "client": 9, "query": { "name": "play" }, "continuation": 0 }),
        ] {
            assert!(
                build_call("client_semantic_find", &arguments(invalid)).is_err(),
                "invalid semantic query survived"
            );
        }
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
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.path.as_deref(), Some("/expects/geometry"));
        assert_eq!(error.expected.expect("expected").kind, ExpectedKind::Absent);
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
        assert_eq!(action, nobox_agent_wire::PointerAction::Click);
        assert_eq!(button, Some(nobox_agent_wire::PointerButton::Left));
        assert!(ensure_visible);

        let defaulted = build_call(
            "client_pointer",
            &arguments(json!({ "client": 3, "x": 4, "y": 5, "action": "click" })),
        )
        .expect("built");
        assert!(matches!(
            defaulted,
            Call::ClientPointer {
                button: Some(nobox_agent_wire::PointerButton::Left),
                ..
            }
        ));

        let missing_scroll_direction = build_call(
            "client_pointer",
            &arguments(json!({ "client": 3, "x": 4, "y": 5, "action": "scroll" })),
        )
        .expect_err("scroll direction is required");
        assert_eq!(missing_scroll_direction.code, ErrorCode::InvalidArgument);
        assert_eq!(missing_scroll_direction.path.as_deref(), Some("/button"));
        assert_eq!(
            missing_scroll_direction.received,
            Some(ReceivedKind::Missing)
        );

        let error = build_call(
            "client_pointer",
            &arguments(json!({ "client": 3, "x": 0, "y": 0, "action": "teleport" })),
        )
        .expect_err("rejected");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.path.as_deref(), Some("/action"));
    }

    #[test]
    fn pointer_schema_matches_its_button_defaults_and_requirements() {
        let pointer = TOOLS
            .iter()
            .find(|tool| tool.name == "client_pointer")
            .expect("pointer tool");
        let schema = (pointer.schema)();

        assert_eq!(schema["properties"]["button"]["default"], "left");
        assert!(schema.get("allOf").is_none());
        assert!(
            schema["properties"]["button"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("required for scroll"))
        );
        assert_eq!(
            schema["properties"]["expects"]["properties"]["content"]["type"],
            "object"
        );
    }

    #[test]
    fn input_observation_schema_and_translation_share_one_bounded_contract() {
        for name in ["client_pointer", "client_key", "client_type"] {
            let tool = TOOLS.iter().find(|tool| tool.name == name).expect("tool");
            let schema = (tool.schema)();
            let observe = &schema["properties"]["observe"];
            assert_eq!(observe["additionalProperties"], false, "{name}");
            assert!(
                !observe["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|field| field == "capture"))
            );
            assert_eq!(
                observe["properties"]["maximum_ms"]["maximum"],
                nobox_agent_wire::MAX_ACTION_OBSERVATION_MS,
                "{name}"
            );
        }

        let call = build_call(
            "client_key",
            &arguments(json!({
                "client": 3,
                "key": "Return",
                "action": "tap",
                "observe": {
                    "capture": {
                        "client": 4,
                        "area": "content",
                        "rect": { "x": 10, "y": 20, "width": 100, "height": 50 },
                        "grid": { "spacing": 50 }
                    },
                    "minimum_ms": 50,
                    "quiet_ms": 100,
                    "maximum_ms": 500
                }
            })),
        )
        .expect("built");
        let Call::ClientKey {
            observe: Some(observe),
            ..
        } = call
        else {
            panic!("observation was dropped");
        };
        assert_eq!(observe.minimum_ms, 50);
        assert_eq!(observe.quiet_ms, 100);
        assert_eq!(observe.maximum_ms, 500);
        let capture = observe.capture.expect("capture");
        assert_eq!(capture.client, Some(ClientId::new(4)));
        assert_eq!(capture.rect.expect("rect").x, 10);
        assert_eq!(capture.grid.expect("grid").spacing, 50);

        let event_only = build_call(
            "client_key",
            &arguments(json!({
                "client": 3,
                "key": "Return",
                "action": "tap",
                "observe": {
                    "minimum_ms": 0,
                    "quiet_ms": 10,
                    "maximum_ms": 100
                }
            })),
        )
        .expect("event-only observation built");
        let Call::ClientKey {
            observe: Some(observe),
            ..
        } = event_only
        else {
            panic!("event-only observation was dropped");
        };
        assert!(observe.capture.is_none());

        let error = build_call(
            "client_key",
            &arguments(json!({
                "client": 3,
                "key": "Return",
                "action": "tap",
                "observe": {
                    "capture": {},
                    "minimum_ms": 0,
                    "quiet_ms": 501,
                    "maximum_ms": 500
                }
            })),
        )
        .expect_err("quiet is bounded by maximum");
        assert_eq!(error.path.as_deref(), Some("/observe/quiet_ms"));
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
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.path.as_deref(), Some("/client"));
        assert_eq!(error.received, Some(ReceivedKind::Missing));
    }

    #[test]
    fn optional_values_with_the_wrong_type_are_not_silently_dropped() {
        let error = build_call(
            "client_set_state",
            &arguments(json!({ "client": 1, "fullscreen": "yes" })),
        )
        .expect_err("rejected");
        assert_eq!(error.path.as_deref(), Some("/fullscreen"));
        assert_eq!(
            error.expected.expect("expected").kind,
            ExpectedKind::Boolean
        );
        assert_eq!(error.received, Some(ReceivedKind::String));
    }

    #[test]
    fn nested_argument_errors_locate_the_exact_field() {
        let error = build_call(
            "client_capture",
            &arguments(json!({
                "client": 7,
                "rect": { "x": 0, "y": 0, "width": "wide", "height": 50 },
            })),
        )
        .expect_err("rejected");
        assert_eq!(error.path.as_deref(), Some("/rect/width"));
        let expected = error.expected.expect("expected");
        assert_eq!(expected.kind, ExpectedKind::Integer);
        assert_eq!(expected.minimum, Some(0));
        assert_eq!(expected.maximum, Some(u64::from(u32::MAX)));
        assert_eq!(error.received, Some(ReceivedKind::String));
    }

    #[test]
    fn mcp_invalid_params_data_is_the_shared_correction_contract() {
        let mut server = disconnected_server();
        let error = server
            .tools_call(&arguments(json!({
                "name": "client_get",
                "arguments": { "client": 7, "window": 9 },
            })))
            .expect_err("rejected before connecting");
        assert_eq!(error["code"], super::mcp::INVALID_PARAMS);
        assert_eq!(error["message"], "Invalid params");
        assert_eq!(error["data"]["code"], "invalid_argument");
        assert_eq!(error["data"]["path"], "/window");
        assert_eq!(error["data"]["expected"]["kind"], "absent");
        assert_eq!(error["data"]["received"], "integer");
        assert_eq!(error["data"]["retryable"], "after_correction");
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error() {
        let error = build_call("rm_rf", &Map::new()).expect_err("rejected");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.path.as_deref(), Some("/name"));
        assert_eq!(error.expected.expect("expected").kind, ExpectedKind::Enum);
    }

    #[test]
    fn a_refusal_is_reported_as_a_tool_error_the_model_can_act_on() {
        let result = tool_refusal(&ProtocolError::denied("no grant"));
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["code"], "denied");
        assert_eq!(
            result["structuredContent"]["retryable"],
            "after_policy_change"
        );
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
        let reply = nobox_agent_wire::Reply::Launched {
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
        let reply = nobox_agent_wire::Reply::Capture {
            image: nobox_agent_wire::CaptureImage {
                format: nobox_agent_wire::ImageFormat::Png,
                width: 8,
                height: 4,
                source: nobox_agent_wire::Rect::new(10, 20, 8, 4),
                content: Some(nobox_agent_wire::Rect::new(0, 0, 8, 4)),
                grid: Some(nobox_agent_wire::AppliedCaptureGrid {
                    spacing: 100,
                    origin_x: 0,
                    origin_y: 0,
                }),
                sequence: nobox_agent_wire::Sequence::new(7),
                data: nobox_agent_wire::Base64Bytes::from(vec![1, 2, 3, 4]),
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
        assert_eq!(result["structuredContent"]["image"]["grid"]["spacing"], 100);

        // And the bytes travel once. Repeating them as text is what made a
        // capture unreadable: hosts truncate the blob and the model goes blind.
        assert!(result["structuredContent"]["image"].get("data").is_none());
        let text = result["content"][1]["text"].as_str().expect("text");
        assert!(!text.contains(encoded));
    }

    #[test]
    fn an_observed_action_hands_its_final_pixels_over_as_an_image_block() {
        let image = nobox_agent_wire::CaptureImage {
            format: nobox_agent_wire::ImageFormat::Png,
            width: 2,
            height: 2,
            source: nobox_agent_wire::Rect::new(0, 0, 2, 2),
            content: Some(nobox_agent_wire::Rect::new(0, 0, 2, 2)),
            grid: None,
            sequence: nobox_agent_wire::Sequence::new(9),
            data: nobox_agent_wire::Base64Bytes::from(vec![1, 2, 3, 4]),
        };
        let reply = nobox_agent_wire::Reply::Injected {
            action: nobox_agent_wire::ActionId::new(2),
            committed: vec![nobox_agent_wire::Step::Inject],
            delivery: nobox_agent_wire::Delivery::Unverified,
            sequence: nobox_agent_wire::Sequence::new(9),
            observation: Some(nobox_agent_wire::ActionObservation {
                started_sequence: nobox_agent_wire::Sequence::new(8),
                finished_sequence: nobox_agent_wire::Sequence::new(9),
                elapsed_ms: 100,
                events: Vec::new(),
                dropped_events: 0,
                samples: vec![nobox_agent_wire::ObservationSample::Ok {
                    after_ms: 100,
                    image,
                }],
            }),
        };

        let result = tool_success(&reply);
        assert_eq!(result["content"][0]["type"], "image");
        assert_eq!(result["structuredContent"]["action"], 2);
        let sample = &result["structuredContent"]["observation"]["samples"][0];
        assert_eq!(sample["status"], "ok");
        assert!(sample["image"].get("data").is_none());
    }
}
