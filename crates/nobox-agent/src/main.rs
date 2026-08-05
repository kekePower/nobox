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
    Call, ClientId, EventKind, Expects, GeometryRequest, KeyAction, Modifier, Outcome,
    PointerButton, Sequence, StateChange, WorkspaceId,
};
use serde_json::{Map, Value, json};

use mcp::{
    INVALID_PARAMS, Incoming, METHOD_NOT_FOUND, PROTOCOL_VERSION, error_object, error_response,
    result_response,
};
use seat::Seat;

/// How long a client may cache `tools/list` and `server/discover`.
const CACHE_TTL_MS: u64 = 300_000;

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
        description: "Return the whole desktop as structured state: outputs, workspaces, \
                      stacking order, focus, and one descriptor per window. Prefer this over \
                      screenshots; it is exact, cheap, and stamped with the sequence number it \
                      corresponds to. Windows the session was not granted, and windows the user \
                      marked hidden, are absent.",
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
        name: "client_pointer",
        title: "Click inside a window",
        description: "Move the pointer to a point inside a window and optionally click, \
                      double-click, or scroll. Coordinates are relative to the window's own \
                      content area, never to the screen, and are translated against the \
                      window's live position at the moment of injection. Set ensure_visible to \
                      activate and raise the window first as one operation. If the user is \
                      typing or clicking, the call is refused as interrupted and reports which \
                      steps had already committed.",
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
                      refused rather than approximated.",
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
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = arguments.next(),
            "--help" | "-h" => {
                println!(
                    "usage: nobox-agent [--socket PATH]\n\n\
                     Speaks MCP {PROTOCOL_VERSION} on stdio and the Agent Seat Protocol to a\n\
                     window manager. The socket is taken from --socket, then\n\
                     AGENT_SEAT_SOCKET, then $XDG_RUNTIME_DIR/nobox/agent-seat-<display>.sock.\n\
                     A manager also advertises it in the _AGENT_SEAT root property."
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
    serve(&socket);
    std::process::ExitCode::SUCCESS
}

/// Runs the stdio loop until the host closes it.
fn serve(socket: &Path) {
    let mut server = Server {
        socket: socket.to_path_buf(),
        seat: None,
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
}

impl Server {
    /// Answers one line of stdio traffic, or nothing for a notification.
    fn handle(&mut self, line: &str) -> Option<String> {
        let incoming = match Incoming::parse(line) {
            Ok(incoming) => incoming,
            Err(error) => return Some(error_response(Value::Null, error).to_string()),
        };
        // Notifications get no reply, including the ones we do not implement.
        let id = incoming.id.clone()?;
        if let Err(error) = incoming.check_protocol() {
            return Some(error_response(id, error).to_string());
        }
        let response = match incoming.method.as_str() {
            "server/discover" => result_response(id, self.discover(), name(), version()),
            "tools/list" => result_response(id, tools_list(), name(), version()),
            "tools/call" => match self.tools_call(&incoming.params) {
                Ok(result) => result_response(id, result, name(), version()),
                Err(error) => error_response(id, error),
            },
            other => error_response(
                id,
                error_object(METHOD_NOT_FOUND, &format!("unknown method: {other}"), None),
            ),
        };
        Some(response.to_string())
    }

    fn discover(&mut self) -> Value {
        // Reporting the seat's own state here makes the first question a host
        // asks also answer "is the window manager there, and what did it
        // actually grant me".
        let seat = match self.connect() {
            Ok(seat) => {
                let welcome = seat.welcome();
                let atoms: Vec<&str> = welcome
                    .granted
                    .atoms()
                    .into_iter()
                    .map(agent_seat_proto::Capability::as_str)
                    .collect();
                format!(
                    "Connected to {} as session {}. Granted: {}. {}",
                    welcome.manager,
                    welcome.session,
                    if atoms.is_empty() {
                        "nothing".to_owned()
                    } else {
                        atoms.join(", ")
                    },
                    if welcome.scoped {
                        "The grant is scoped: windows outside it are absent, not merely inert."
                    } else {
                        "The grant is not scoped to an application."
                    }
                )
            }
            Err(error) => format!("Not connected to a window manager: {error}"),
        };
        json!({
            "supportedVersions": [PROTOCOL_VERSION],
            "capabilities": { "tools": {} },
            "instructions": format!(
                "This server exposes a window manager's agent seat: structured desktop state \
                 instead of screenshots, and window-addressed actions instead of global input. \
                 Capabilities are granted by the user, per companion executable, and every \
                 request is checked by the window manager itself. {seat}"
            ),
            "ttlMs": CACHE_TTL_MS,
            "cacheScope": "private",
        })
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
    let structured = serde_json::to_value(reply).unwrap_or(Value::Null);
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": false,
    })
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
}
