//! Handshake, tool calls, replies, and events.

use serde::{Deserialize, Serialize};

use crate::base64::Base64Bytes;
use crate::capability::{Bundle, CapabilitySet};
use crate::error::{ErrorCode, ProtocolError};
use crate::ids::{
    ClientId, Generation, OutputId, Rect, RequestId, Sequence, SessionId, WorkspaceId,
};
use crate::{PROTOCOL_NAME, PROTOCOL_VERSION};

/// Longest accepted declared harness name.
pub const MAX_HARNESS_LEN: usize = 128;
/// Longest accepted declared purpose string.
pub const MAX_PURPOSE_LEN: usize = 512;
/// Longest accepted text for one `client.type` call.
pub const MAX_TYPE_TEXT_LEN: usize = 4096;
/// Longest accepted key name.
pub const MAX_KEY_NAME_LEN: usize = 64;
/// Longest accepted desktop-entry identifier.
pub const MAX_DESKTOP_ENTRY_LEN: usize = 256;
/// Longest accepted launch URI.
pub const MAX_URI_LEN: usize = 2048;
/// Most URIs one launch may carry.
pub const MAX_LAUNCH_URIS: usize = 16;
/// Most modifiers one key call may carry.
pub const MAX_MODIFIERS: usize = 8;

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// The first frame a companion sends.
///
/// Every string here is declared by the agent side and is display-only: it is
/// shown in consent and tracing, and is never an authorization input. The
/// manager authorizes against verified peer identity instead.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Protocol name the companion speaks; must equal [`PROTOCOL_NAME`].
    pub protocol: String,
    /// Protocol version the companion speaks; must equal [`PROTOCOL_VERSION`].
    pub version: u32,
    /// Declared harness name, for display only.
    pub harness: String,
    /// Declared purpose, for display only.
    pub purpose: String,
    /// Capability bundles the agent would like to hold. Requesting is not
    /// receiving: the manager answers with what it actually granted.
    #[serde(default)]
    pub requested: Vec<Bundle>,
}

impl Hello {
    /// Builds a hello for this build of the protocol.
    #[must_use]
    pub fn new(harness: impl Into<String>, purpose: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_owned(),
            version: PROTOCOL_VERSION,
            harness: harness.into(),
            purpose: purpose.into(),
            requested: Vec::new(),
        }
    }

    /// Checks the protocol name, version, and declared-string bounds.
    ///
    /// # Errors
    ///
    /// Returns a fatal [`ProtocolError`] when the peer speaks a different
    /// protocol or exceeds a declared-identity bound.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol != PROTOCOL_NAME || self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::new(
                ErrorCode::UnsupportedVersion,
                format!("this manager speaks {PROTOCOL_NAME} version {PROTOCOL_VERSION}"),
            ));
        }
        if self.harness.is_empty() || self.harness.len() > MAX_HARNESS_LEN {
            return Err(ProtocolError::new(
                ErrorCode::InvalidIdentity,
                "declared harness name is empty or too long",
            ));
        }
        if self.purpose.len() > MAX_PURPOSE_LEN {
            return Err(ProtocolError::new(
                ErrorCode::InvalidIdentity,
                "declared purpose is too long",
            ));
        }
        if self.harness.chars().any(char::is_control) || self.purpose.chars().any(char::is_control)
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidIdentity,
                "declared identity contains control characters",
            ));
        }
        Ok(())
    }
}

/// The manager's answer to a hello.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    /// Protocol name the manager speaks.
    pub protocol: String,
    /// Protocol version the manager speaks.
    pub version: u32,
    /// Implementation name and version, for display and tracing.
    pub manager: String,
    /// Session identity, unique for the manager's lifetime.
    pub session: SessionId,
    /// Per-connection nonce. A session cannot be resumed or replayed, so this
    /// value is never valid on a later connection.
    pub nonce: String,
    /// Capability atoms actually granted. Empty means deny-by-default: the
    /// session is live, and every capability request will be refused.
    pub granted: CapabilitySet,
    /// Whether the grant is restricted to an application scope. Out-of-scope
    /// clients are absent from every response and event, not merely inert.
    pub scoped: bool,
    /// Sequence number the session starts from.
    pub sequence: Sequence,
    /// Optional backend-dependent features this manager can actually perform.
    #[serde(default)]
    pub features: Vec<Feature>,
}

/// A backend-dependent ability an agent may not assume from the grant alone.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Capture of an obscured client is possible, through composite
    /// redirection or compositor cooperation.
    ObscuredCapture,
    /// Whole-output capture is possible.
    OutputCapture,
    /// Synthesized input is possible.
    InputInjection,
    /// Launching from the desktop-entry catalog is possible.
    DesktopLaunch,
}

/// The manager's final frame before closing a session.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Goodbye {
    /// Why the session ended.
    pub reason: DisconnectReason,
    /// Human-readable detail.
    pub message: String,
}

/// Why the manager closed a session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectReason {
    /// The companion sent a frame the manager could not accept.
    ProtocolViolation,
    /// The companion could not keep up with its bounded event queue.
    SlowConsumer,
    /// The grant was revoked, by configuration reload or by the human.
    Revoked,
    /// The manager is shutting down.
    ManagerShutdown,
}

// ---------------------------------------------------------------------------
// Descriptors
// ---------------------------------------------------------------------------

/// Application identity used for rule matching and display. Field names match
/// the manager's application-rule matcher exactly, so a scope expressed in
/// configuration and an identity observed by an agent describe one thing.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationIdentity {
    /// Application instance/name.
    pub name: Option<String>,
    /// Application class.
    pub class: Option<String>,
    /// Window-group leader instance/name.
    pub group_name: Option<String>,
    /// Window-group leader class.
    pub group_class: Option<String>,
    /// Toolkit/application role string.
    pub role: Option<String>,
    /// Functional top-level type.
    pub kind: ApplicationKind,
}

/// Functional type of a top-level client.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationKind {
    /// Ordinary application window.
    #[default]
    Normal,
    /// Dialog.
    Dialog,
    /// Utility window.
    Utility,
    /// Toolbar.
    Toolbar,
    /// Menu.
    Menu,
    /// Splash window.
    Splash,
    /// Desktop surface.
    Desktop,
    /// Dock or panel.
    Dock,
    /// Drop-down menu.
    DropdownMenu,
    /// Pop-up menu.
    PopupMenu,
    /// Tooltip.
    Tooltip,
    /// Notification.
    Notification,
    /// Combo-box pop-up.
    Combo,
    /// Drag-and-drop surface.
    DragAndDrop,
}

/// Boolean state flags of one client.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientState {
    /// Holds the input focus.
    pub focused: bool,
    /// Currently displayed on an output.
    pub visible: bool,
    /// Iconified.
    pub minimized: bool,
    /// Maximized horizontally.
    pub maximized_horizontal: bool,
    /// Maximized vertically.
    pub maximized_vertical: bool,
    /// Fullscreen.
    pub fullscreen: bool,
    /// Shaded to its titlebar.
    pub shaded: bool,
    /// Present on every workspace.
    pub sticky: bool,
    /// Kept above ordinary windows.
    pub above: bool,
    /// Kept below ordinary windows.
    pub below: bool,
    /// Requesting attention.
    pub urgent: bool,
    /// Modal for its parent.
    pub modal: bool,
    /// Drawn with manager decorations.
    pub decorated: bool,
}

/// One client as the agent sees it.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientDescriptor {
    /// Protocol identity.
    pub client: ClientId,
    /// Counter bumped on any descriptor-visible change.
    pub generation: Generation,
    /// Application identity.
    pub application: ApplicationIdentity,
    /// Current title. Absent when the session lacks `observe.titles`, and
    /// absent for redacted clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether an application rule redacts this client. Redacted clients
    /// appear with existence and geometry but no title, and refuse capture
    /// and input. Hidden clients never appear at all, so this flag can never
    /// be true for them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
    /// Content rectangle in root coordinates.
    pub content: Rect,
    /// Decorated frame rectangle in root coordinates.
    pub frame: Rect,
    /// Workspace, or absent when the client is on every workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceId>,
    /// Output the manager considers this client to be on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputId>,
    /// State flags.
    pub state: ClientState,
    /// Parent, for a transient with a specific parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transient_for: Option<ClientId>,
}

/// One connected output.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDescriptor {
    /// Protocol identity.
    pub output: OutputId,
    /// Backend-reported name, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Full output rectangle.
    pub geometry: Rect,
    /// Rectangle left after panels reserve space.
    pub work_area: Rect,
    /// Whether this is the primary output.
    pub primary: bool,
}

/// One workspace.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDescriptor {
    /// Protocol identity.
    pub workspace: WorkspaceId,
    /// Configured name, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The whole world model in one value, stamped with the sequence it holds for.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSnapshot {
    /// Sequence this snapshot corresponds to.
    pub sequence: Sequence,
    /// Connected outputs.
    pub outputs: Vec<OutputDescriptor>,
    /// Workspaces.
    pub workspaces: Vec<WorkspaceDescriptor>,
    /// Currently displayed workspace.
    pub current_workspace: WorkspaceId,
    /// Focused client, when one holds focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<ClientId>,
    /// Stacking order, bottom to top, restricted to the session's scope.
    pub stacking: Vec<ClientId>,
    /// Client descriptors, restricted to the session's scope.
    pub clients: Vec<ClientDescriptor>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Event kinds, usable as a subscription filter.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A client appeared.
    ClientMapped,
    /// A client went away.
    ClientClosed,
    /// A title changed.
    TitleChanged,
    /// The focused client changed.
    FocusChanged,
    /// State flags changed.
    StateChanged,
    /// Geometry settled at a new rectangle.
    GeometryChanged,
    /// The displayed workspace changed.
    WorkspaceSwitched,
    /// The human used the pointer or keyboard.
    HumanActivity,
    /// Session lifecycle.
    SessionControl,
    /// The session's queue overflowed and the world model must be rebuilt.
    ResyncRequired,
}

impl EventKind {
    /// Every event kind.
    pub const ALL: [Self; 10] = [
        Self::ClientMapped,
        Self::ClientClosed,
        Self::TitleChanged,
        Self::FocusChanged,
        Self::StateChanged,
        Self::GeometryChanged,
        Self::WorkspaceSwitched,
        Self::HumanActivity,
        Self::SessionControl,
        Self::ResyncRequired,
    ];

    /// Returns whether this kind may be filtered out by a subscription.
    /// Session control and resync are always delivered: an agent must never be
    /// able to filter away the news that its world model is invalid.
    #[must_use]
    pub const fn is_filterable(self) -> bool {
        !matches!(self, Self::SessionControl | Self::ResyncRequired)
    }
}

/// What the human did, without any content.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanActivityKind {
    /// Pointer motion, button, or scroll.
    Pointer,
    /// Key press or release.
    Keyboard,
}

/// Why a session's lifecycle changed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionChange {
    /// Frozen by the kill chord. Freezing is not revocation: the human decides
    /// afterward whether the session resumes or ends.
    Frozen,
    /// Resumed by the human after a freeze.
    Resumed,
    /// Grant withdrawn; no further capability request will succeed.
    Revoked,
}

/// One event, without its sequence stamp.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    /// A client appeared, with the token of the launch that produced it when
    /// this manager launched it.
    ClientMapped {
        /// Full descriptor of the new client. Boxed so one large variant does
        /// not set the size of every queued event.
        client: Box<ClientDescriptor>,
        /// Correlation token returned by the launch that produced it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch: Option<String>,
    },
    /// A client went away.
    ClientClosed {
        /// The client that is gone.
        client: ClientId,
    },
    /// A title changed.
    TitleChanged {
        /// Affected client.
        client: ClientId,
        /// Generation after the change.
        generation: Generation,
        /// New title, absent when withheld.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// The focused client changed.
    FocusChanged {
        /// Newly focused client, absent when focus left every client.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<ClientId>,
    },
    /// State flags changed.
    StateChanged {
        /// Affected client.
        client: ClientId,
        /// Generation after the change.
        generation: Generation,
        /// Complete flags after the change.
        state: ClientState,
    },
    /// Geometry settled. Interactive drags emit the settled result, not the
    /// storm of intermediate rectangles.
    GeometryChanged {
        /// Affected client.
        client: ClientId,
        /// Generation after the change.
        generation: Generation,
        /// Content rectangle after the change.
        content: Rect,
        /// Frame rectangle after the change.
        frame: Rect,
    },
    /// The displayed workspace changed.
    WorkspaceSwitched {
        /// Newly displayed workspace.
        workspace: WorkspaceId,
    },
    /// The human used an input device. Carries no content, only that it
    /// happened, so an observing agent cannot read the human's input.
    HumanActivity {
        /// Which device class.
        kind: HumanActivityKind,
    },
    /// The session's lifecycle changed.
    SessionControl {
        /// What happened.
        change: SessionChange,
    },
    /// The session's bounded queue overflowed. The backlog is gone and the
    /// agent must re-snapshot; slow consumers degrade only themselves.
    ResyncRequired {
        /// How many events were dropped.
        dropped: u64,
    },
}

impl Event {
    /// Returns the kind of this event, for filtering.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::ClientMapped { .. } => EventKind::ClientMapped,
            Self::ClientClosed { .. } => EventKind::ClientClosed,
            Self::TitleChanged { .. } => EventKind::TitleChanged,
            Self::FocusChanged { .. } => EventKind::FocusChanged,
            Self::StateChanged { .. } => EventKind::StateChanged,
            Self::GeometryChanged { .. } => EventKind::GeometryChanged,
            Self::WorkspaceSwitched { .. } => EventKind::WorkspaceSwitched,
            Self::HumanActivity { .. } => EventKind::HumanActivity,
            Self::SessionControl { .. } => EventKind::SessionControl,
            Self::ResyncRequired { .. } => EventKind::ResyncRequired,
        }
    }
}

/// An event with its place in the manager's single monotonic sequence.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    /// Sequence stamp.
    pub sequence: Sequence,
    /// The event itself.
    pub event: Event,
}

// ---------------------------------------------------------------------------
// Call arguments
// ---------------------------------------------------------------------------

/// State the agent believes it is acting on. The manager refuses with
/// [`ErrorCode::StaleState`] rather than acting on obsolete assumptions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Expects {
    /// The client's generation when the agent last observed it.
    pub generation: Option<Generation>,
    /// The content rectangle the agent last observed.
    pub content: Option<Rect>,
    /// The workspace the agent last observed the client on.
    pub workspace: Option<WorkspaceId>,
    /// Whether the client was focused.
    pub focused: Option<bool>,
}

impl Expects {
    /// Returns whether any precondition is set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.generation.is_none()
            && self.content.is_none()
            && self.workspace.is_none()
            && self.focused.is_none()
    }
}

/// One committed unit of a multi-step operation. Results name these exactly,
/// so a preempted sequence reports where it stopped instead of reporting
/// success or silence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The workspace holding the target was switched to.
    WorkspaceSwitch,
    /// The client was activated through the focus contract.
    Activate,
    /// The client was raised.
    Raise,
    /// Geometry was applied.
    Geometry,
    /// State flags were applied.
    State,
    /// Workspace assignment was applied.
    Assign,
    /// A close was negotiated with the client.
    Close,
    /// Input was injected.
    Inject,
    /// An application was started.
    Launch,
}

/// Pointer action for a window-addressed pointer call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerAction {
    /// Move only.
    Move,
    /// Press and hold.
    Press,
    /// Release.
    Release,
    /// Press and release.
    Click,
    /// Two clicks within the double-click interval.
    DoubleClick,
    /// Scroll.
    Scroll,
}

/// Pointer button, named rather than numbered so the protocol stays neutral.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    /// Primary button.
    Left,
    /// Middle button.
    Middle,
    /// Secondary button.
    Right,
    /// Scroll up.
    ScrollUp,
    /// Scroll down.
    ScrollDown,
    /// Scroll left.
    ScrollLeft,
    /// Scroll right.
    ScrollRight,
}

/// Key action for a window-addressed key call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    /// Press and hold.
    Press,
    /// Release.
    Release,
    /// Press and release.
    Tap,
}

/// A modifier held during a key call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    /// Shift.
    Shift,
    /// Control.
    Control,
    /// Alt.
    Alt,
    /// Super/meta.
    Super,
    /// AltGr.
    AltGr,
}

/// Requested state changes. Absent fields are left alone.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StateChange {
    /// Iconify or restore.
    pub minimized: Option<bool>,
    /// Maximize horizontally.
    pub maximized_horizontal: Option<bool>,
    /// Maximize vertically.
    pub maximized_vertical: Option<bool>,
    /// Fullscreen.
    pub fullscreen: Option<bool>,
    /// Shade.
    pub shaded: Option<bool>,
    /// Show on every workspace.
    pub sticky: Option<bool>,
    /// Keep above.
    pub above: Option<bool>,
    /// Keep below.
    pub below: Option<bool>,
}

impl StateChange {
    /// Returns whether the change would alter nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.minimized.is_none()
            && self.maximized_horizontal.is_none()
            && self.maximized_vertical.is_none()
            && self.fullscreen.is_none()
            && self.shaded.is_none()
            && self.sticky.is_none()
            && self.above.is_none()
            && self.below.is_none()
    }
}

/// A partial geometry request. Absent fields keep their current value, and the
/// manager applies the same constraints as any other geometry source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeometryRequest {
    /// Requested horizontal position.
    pub x: Option<i32>,
    /// Requested vertical position.
    pub y: Option<i32>,
    /// Requested width.
    pub width: Option<u32>,
    /// Requested height.
    pub height: Option<u32>,
}

impl GeometryRequest {
    /// Returns whether the request would change nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.x.is_none() && self.y.is_none() && self.width.is_none() && self.height.is_none()
    }
}

/// Which rectangle of a client to capture.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureArea {
    /// The client's content rectangle.
    #[default]
    Content,
    /// The decorated frame rectangle.
    Frame,
}

/// Encoding of returned pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    /// PNG.
    Png,
}

/// Captured pixels, stamped with what they are pixels of and when.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureImage {
    /// Encoding of `data`.
    pub format: ImageFormat,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Rectangle these pixels were taken from, in root coordinates.
    pub source: Rect,
    /// Sequence the capture corresponds to.
    pub sequence: Sequence,
    /// Encoded image bytes.
    pub data: Base64Bytes,
}

// ---------------------------------------------------------------------------
// Calls and replies
// ---------------------------------------------------------------------------

/// One tool call. All input is window-addressed: global coordinates are
/// inexpressible in this protocol, by construction rather than by policy.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "tool", deny_unknown_fields)]
pub enum Call {
    /// Returns the whole world model.
    #[serde(rename = "desktop.snapshot")]
    DesktopSnapshot {},
    /// Returns one client descriptor.
    #[serde(rename = "client.get")]
    ClientGet {
        /// Target client.
        client: ClientId,
    },
    /// Begins buffering events and returns the snapshot they continue from, as
    /// one operation at a single event-loop boundary.
    #[serde(rename = "subscribe_and_snapshot")]
    SubscribeAndSnapshot {
        /// Kinds to deliver. Empty means every kind. Unfilterable kinds are
        /// delivered regardless.
        #[serde(default)]
        kinds: Vec<EventKind>,
    },
    /// Captures one client.
    #[serde(rename = "client.capture")]
    ClientCapture {
        /// Target client.
        client: ClientId,
        /// Which rectangle to capture.
        #[serde(default)]
        area: CaptureArea,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Captures one whole output.
    #[serde(rename = "output.capture")]
    OutputCapture {
        /// Target output.
        output: OutputId,
    },
    /// Injects a pointer action at content-relative coordinates.
    #[serde(rename = "client.pointer")]
    ClientPointer {
        /// Target client.
        client: ClientId,
        /// Content-relative horizontal position.
        x: i32,
        /// Content-relative vertical position.
        y: i32,
        /// What the pointer does.
        action: PointerAction,
        /// Which button, for button and scroll actions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<PointerButton>,
        /// Activate and raise the target first, as one serialized operation.
        #[serde(default)]
        ensure_visible: bool,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Injects one key.
    #[serde(rename = "client.key")]
    ClientKey {
        /// Target client.
        client: ClientId,
        /// Key name.
        key: String,
        /// What the key does.
        action: KeyAction,
        /// Modifiers held for the duration.
        #[serde(default)]
        modifiers: Vec<Modifier>,
        /// Activate and raise the target first.
        #[serde(default)]
        ensure_visible: bool,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Types text into a client.
    #[serde(rename = "client.type")]
    ClientType {
        /// Target client.
        client: ClientId,
        /// Text to type.
        text: String,
        /// Activate and raise the target first.
        #[serde(default)]
        ensure_visible: bool,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Activates a client through the manager's focus contract.
    #[serde(rename = "client.activate")]
    ClientActivate {
        /// Target client.
        client: ClientId,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Asks a client to close, using its own negotiation only. There is no
    /// forced kill in this protocol.
    #[serde(rename = "client.close")]
    ClientClose {
        /// Target client.
        client: ClientId,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Moves and resizes a client.
    #[serde(rename = "client.move_resize")]
    ClientMoveResize {
        /// Target client.
        client: ClientId,
        /// Requested geometry.
        geometry: GeometryRequest,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Changes a client's state flags.
    #[serde(rename = "client.set_state")]
    ClientSetState {
        /// Target client.
        client: ClientId,
        /// Requested changes.
        change: StateChange,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Sends a client to a workspace.
    #[serde(rename = "client.send_to_workspace")]
    ClientSendToWorkspace {
        /// Target client.
        client: ClientId,
        /// Destination workspace.
        workspace: WorkspaceId,
        /// Also switch the display to that workspace.
        #[serde(default)]
        follow: bool,
        /// Freshness preconditions.
        #[serde(default)]
        expects: Expects,
    },
    /// Switches the displayed workspace.
    #[serde(rename = "workspace.switch")]
    WorkspaceSwitch {
        /// Destination workspace.
        workspace: WorkspaceId,
    },
    /// Starts an application from the desktop-entry catalog. Never a shell
    /// string: only catalog identifiers are expressible.
    #[serde(rename = "launch")]
    Launch {
        /// Desktop-entry identifier.
        desktop_entry: String,
        /// URIs or files to pass through the entry's own expansion.
        #[serde(default)]
        uris: Vec<String>,
    },
}

impl Call {
    /// Returns the wire name of this tool.
    #[must_use]
    pub const fn tool(&self) -> &'static str {
        match self {
            Self::DesktopSnapshot {} => "desktop.snapshot",
            Self::ClientGet { .. } => "client.get",
            Self::SubscribeAndSnapshot { .. } => "subscribe_and_snapshot",
            Self::ClientCapture { .. } => "client.capture",
            Self::OutputCapture { .. } => "output.capture",
            Self::ClientPointer { .. } => "client.pointer",
            Self::ClientKey { .. } => "client.key",
            Self::ClientType { .. } => "client.type",
            Self::ClientActivate { .. } => "client.activate",
            Self::ClientClose { .. } => "client.close",
            Self::ClientMoveResize { .. } => "client.move_resize",
            Self::ClientSetState { .. } => "client.set_state",
            Self::ClientSendToWorkspace { .. } => "client.send_to_workspace",
            Self::WorkspaceSwitch { .. } => "workspace.switch",
            Self::Launch { .. } => "launch",
        }
    }

    /// Returns every capability atom this call needs. A call is refused unless
    /// the session holds all of them; no capability implies another.
    #[must_use]
    pub fn required(&self) -> CapabilitySet {
        use crate::capability::Capability as C;
        let atoms: &[C] = match self {
            Self::DesktopSnapshot {}
            | Self::ClientGet { .. }
            | Self::SubscribeAndSnapshot { .. } => &[C::ObserveStructure],
            Self::ClientCapture { .. } => &[C::ObserveStructure, C::CaptureClientVisible],
            Self::OutputCapture { .. } => &[C::CaptureOutput],
            Self::ClientPointer { .. } => &[C::ObserveStructure, C::InputPointer],
            Self::ClientKey { .. } | Self::ClientType { .. } => {
                &[C::ObserveStructure, C::InputKeyboard]
            }
            Self::ClientActivate { .. } => &[C::ObserveStructure, C::ManageActivate],
            Self::ClientClose { .. } => &[C::ObserveStructure, C::ManageClose],
            Self::ClientMoveResize { .. } => &[C::ObserveStructure, C::ManageGeometry],
            Self::ClientSetState { .. } => &[C::ObserveStructure, C::ManageState],
            Self::ClientSendToWorkspace { .. } | Self::WorkspaceSwitch { .. } => {
                &[C::ManageWorkspace]
            }
            Self::Launch { .. } => &[C::LaunchDesktop],
        };
        let mut set = CapabilitySet::from_iter_atoms(atoms.iter().copied());
        if matches!(
            self,
            Self::ClientPointer {
                ensure_visible: true,
                ..
            } | Self::ClientKey {
                ensure_visible: true,
                ..
            } | Self::ClientType {
                ensure_visible: true,
                ..
            }
        ) {
            set = set.with(C::ManageActivate);
        }
        if matches!(self, Self::ClientSendToWorkspace { follow: true, .. }) {
            set = set.with(C::ManageWorkspace);
        }
        // Stickiness is workspace membership however it is spelled, so it
        // needs the workspace capability and not merely the state one.
        if matches!(
            self,
            Self::ClientSetState {
                change: StateChange {
                    sticky: Some(_),
                    ..
                },
                ..
            }
        ) {
            set = set.with(C::ManageWorkspace);
        }
        set
    }

    /// Checks argument bounds that the manager must not have to guess at.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidArgument`] when an argument exceeds a
    /// protocol bound or is internally inconsistent.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let invalid = |message: &str| ProtocolError::new(ErrorCode::InvalidArgument, message);
        match self {
            Self::SubscribeAndSnapshot { kinds } => {
                if kinds.len() > EventKind::ALL.len() {
                    return Err(invalid("too many event kinds"));
                }
            }
            Self::ClientPointer { action, button, .. } => {
                let needs_button = !matches!(action, PointerAction::Move);
                if needs_button && button.is_none() {
                    return Err(invalid("this pointer action requires a button"));
                }
                let scrolling = matches!(action, PointerAction::Scroll);
                let scroll_button = matches!(
                    button,
                    Some(
                        PointerButton::ScrollUp
                            | PointerButton::ScrollDown
                            | PointerButton::ScrollLeft
                            | PointerButton::ScrollRight
                    )
                );
                if scrolling != scroll_button && button.is_some() {
                    return Err(invalid("scroll actions require a scroll button"));
                }
            }
            Self::ClientKey { key, modifiers, .. } => {
                if key.is_empty() || key.len() > MAX_KEY_NAME_LEN {
                    return Err(invalid("key name is empty or too long"));
                }
                if modifiers.len() > MAX_MODIFIERS {
                    return Err(invalid("too many modifiers"));
                }
            }
            Self::ClientType { text, .. } => {
                if text.is_empty() || text.len() > MAX_TYPE_TEXT_LEN {
                    return Err(invalid("text is empty or too long"));
                }
            }
            Self::ClientMoveResize { geometry, .. } => {
                if geometry.is_empty() {
                    return Err(invalid("geometry request changes nothing"));
                }
                if geometry.width == Some(0) || geometry.height == Some(0) {
                    return Err(invalid("geometry request has a zero extent"));
                }
            }
            Self::ClientSetState { change, .. } => {
                if change.is_empty() {
                    return Err(invalid("state change changes nothing"));
                }
            }
            Self::Launch {
                desktop_entry,
                uris,
            } => {
                if desktop_entry.is_empty() || desktop_entry.len() > MAX_DESKTOP_ENTRY_LEN {
                    return Err(invalid("desktop-entry identifier is empty or too long"));
                }
                if uris.len() > MAX_LAUNCH_URIS {
                    return Err(invalid("too many launch arguments"));
                }
                if uris.iter().any(|uri| uri.len() > MAX_URI_LEN) {
                    return Err(invalid("launch argument is too long"));
                }
            }
            Self::DesktopSnapshot {}
            | Self::ClientGet { .. }
            | Self::ClientCapture { .. }
            | Self::OutputCapture { .. }
            | Self::ClientActivate { .. }
            | Self::ClientClose { .. }
            | Self::ClientSendToWorkspace { .. }
            | Self::WorkspaceSwitch { .. } => {}
        }
        Ok(())
    }
}

/// A successful result.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// The world model.
    Snapshot {
        /// The snapshot.
        snapshot: DesktopSnapshot,
    },
    /// One client descriptor.
    Client {
        /// The descriptor.
        client: ClientDescriptor,
    },
    /// A subscription, plus the snapshot its stream continues from.
    Subscribed {
        /// Kinds that will be delivered.
        kinds: Vec<EventKind>,
        /// Snapshot taken at the same event-loop boundary.
        snapshot: DesktopSnapshot,
    },
    /// Captured pixels.
    Capture {
        /// The image.
        image: CaptureImage,
    },
    /// An application was started.
    Launched {
        /// Correlation token that the resulting `client_mapped` event carries.
        launch: String,
    },
    /// A mutating call finished, naming everything it committed.
    Committed {
        /// Steps that were performed, in order.
        committed: Vec<Step>,
        /// Sequence after the change.
        sequence: Sequence,
    },
}

/// The result of one request: success with a reply, or a structured failure.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    /// The call succeeded.
    Ok {
        /// What it produced.
        reply: Reply,
    },
    /// The call failed.
    Error {
        /// Why, and what committed before it failed.
        error: ProtocolError,
    },
}

impl Outcome {
    /// Returns whether this outcome is a success.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Returns the error code, when this outcome is a failure.
    #[must_use]
    pub const fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Ok { .. } => None,
            Self::Error { error } => Some(error.code),
        }
    }
}

/// One request from the companion.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Agent-chosen identifier, echoed on the response.
    pub id: RequestId,
    /// The tool call.
    pub call: Call,
}

/// One response from the manager.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Identifier of the request being answered.
    pub id: RequestId,
    /// Sequence the manager was at when it answered.
    pub sequence: Sequence,
    /// Success or structured failure.
    pub outcome: Outcome,
}

/// Anything the companion can send.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    /// The handshake, which must be the first frame and may not repeat.
    Hello(Hello),
    /// A tool call.
    Request(Request),
}

/// Anything the manager can send.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMessage {
    /// The answer to a handshake.
    Welcome(Welcome),
    /// The answer to a request.
    Response(Response),
    /// A pushed event.
    Event(EventEnvelope),
    /// The final frame before the manager closes the session.
    Goodbye(Goodbye),
}

#[cfg(test)]
mod tests {
    use super::{
        Call, ClientMessage, ErrorCode, Event, EventKind, Expects, Hello, Outcome, PointerAction,
        PointerButton, Request, Response, ServerMessage, Step,
    };
    use crate::capability::{Capability, CapabilitySet};
    use crate::error::ProtocolError;
    use crate::ids::{ClientId, Generation, RequestId, Sequence, WorkspaceId};
    use crate::{PROTOCOL_NAME, PROTOCOL_VERSION};

    fn round_trip<T>(value: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let encoded = serde_json::to_string(value).expect("encodes");
        serde_json::from_str(&encoded).expect("decodes")
    }

    #[test]
    fn hello_defaults_to_this_protocol_build() {
        let hello = Hello::new("example-harness", "regression test");
        assert_eq!(hello.protocol, PROTOCOL_NAME);
        assert_eq!(hello.version, PROTOCOL_VERSION);
        hello.validate().expect("valid");
        assert_eq!(round_trip(&hello), hello);
    }

    #[test]
    fn hello_rejects_other_protocols_and_versions() {
        let mut hello = Hello::new("h", "p");
        hello.version = PROTOCOL_VERSION + 1;
        assert_eq!(
            hello.validate().expect_err("mismatch").code,
            ErrorCode::UnsupportedVersion
        );
        let mut hello = Hello::new("h", "p");
        hello.protocol = "something-else".to_owned();
        assert_eq!(
            hello.validate().expect_err("mismatch").code,
            ErrorCode::UnsupportedVersion
        );
    }

    #[test]
    fn hello_bounds_declared_identity() {
        let mut hello = Hello::new(String::new(), "p");
        assert_eq!(
            hello.validate().expect_err("empty").code,
            ErrorCode::InvalidIdentity
        );
        hello = Hello::new("x".repeat(super::MAX_HARNESS_LEN + 1), "p");
        assert_eq!(
            hello.validate().expect_err("long").code,
            ErrorCode::InvalidIdentity
        );
        hello = Hello::new("harness\nname", "p");
        assert_eq!(
            hello.validate().expect_err("control").code,
            ErrorCode::InvalidIdentity
        );
    }

    #[test]
    fn tool_names_are_dotted_on_the_wire() {
        let call = Call::ClientGet {
            client: ClientId::new(3),
        };
        let encoded = serde_json::to_string(&call).expect("encodes");
        assert_eq!(encoded, "{\"tool\":\"client.get\",\"client\":3}");
        assert_eq!(call.tool(), "client.get");
        assert_eq!(round_trip(&call), call);
    }

    #[test]
    fn every_call_reports_the_tool_name_it_encodes() {
        let calls = [
            Call::DesktopSnapshot {},
            Call::ClientGet {
                client: ClientId::new(1),
            },
            Call::SubscribeAndSnapshot { kinds: Vec::new() },
            Call::WorkspaceSwitch {
                workspace: WorkspaceId::new(2),
            },
            Call::Launch {
                desktop_entry: "org.example.App.desktop".to_owned(),
                uris: Vec::new(),
            },
        ];
        for call in calls {
            let value = serde_json::to_value(&call).expect("encodes");
            assert_eq!(value["tool"], call.tool());
            assert_eq!(round_trip(&call), call);
        }
    }

    #[test]
    fn unknown_call_arguments_are_rejected() {
        let decoded = serde_json::from_str::<Call>(
            "{\"tool\":\"client.get\",\"client\":1,\"window\":\"0x400001\"}",
        );
        assert!(decoded.is_err());
    }

    #[test]
    fn unknown_tools_are_rejected() {
        let decoded = serde_json::from_str::<Call>("{\"tool\":\"pointer.move_global\",\"x\":0}");
        assert!(decoded.is_err());
    }

    #[test]
    fn calls_require_only_the_atoms_they_use() {
        let observe = Call::DesktopSnapshot {}.required();
        assert!(observe.holds(Capability::ObserveStructure));
        assert!(!observe.holds(Capability::ObserveTitles));
        assert!(!observe.holds(Capability::ManageActivate));

        let click = Call::ClientPointer {
            client: ClientId::new(1),
            x: 4,
            y: 4,
            action: PointerAction::Click,
            button: Some(PointerButton::Left),
            ensure_visible: false,
            expects: Expects::default(),
        };
        assert!(click.required().holds(Capability::InputPointer));
        assert!(!click.required().holds(Capability::ManageActivate));
    }

    #[test]
    fn stickiness_needs_the_workspace_capability_however_it_is_spelled() {
        let call = Call::ClientSetState {
            client: ClientId::new(1),
            change: super::StateChange {
                sticky: Some(true),
                ..super::StateChange::default()
            },
            expects: Expects::default(),
        };
        assert!(call.required().holds(Capability::ManageWorkspace));
        let shading = Call::ClientSetState {
            client: ClientId::new(1),
            change: super::StateChange {
                shaded: Some(true),
                ..super::StateChange::default()
            },
            expects: Expects::default(),
        };
        assert!(!shading.required().holds(Capability::ManageWorkspace));
        assert!(shading.required().holds(Capability::ManageState));
    }

    #[test]
    fn ensure_visible_additionally_requires_activation() {
        let call = Call::ClientType {
            client: ClientId::new(1),
            text: "hello".to_owned(),
            ensure_visible: true,
            expects: Expects::default(),
        };
        let required = call.required();
        assert!(required.holds(Capability::InputKeyboard));
        assert!(required.holds(Capability::ManageActivate));
    }

    #[test]
    fn no_call_is_satisfied_by_an_empty_grant() {
        let calls = [
            Call::DesktopSnapshot {},
            Call::ClientGet {
                client: ClientId::new(1),
            },
            Call::WorkspaceSwitch {
                workspace: WorkspaceId::new(0),
            },
            Call::Launch {
                desktop_entry: "a.desktop".to_owned(),
                uris: Vec::new(),
            },
        ];
        for call in calls {
            let required = call.required();
            assert!(!required.is_empty(), "{} requires nothing", call.tool());
            assert_ne!(
                required.intersection(CapabilitySet::EMPTY),
                required,
                "{} would pass an empty grant",
                call.tool()
            );
        }
    }

    #[test]
    fn call_validation_bounds_arguments() {
        let long_text = Call::ClientType {
            client: ClientId::new(1),
            text: "x".repeat(super::MAX_TYPE_TEXT_LEN + 1),
            ensure_visible: false,
            expects: Expects::default(),
        };
        assert_eq!(
            long_text.validate().expect_err("too long").code,
            ErrorCode::InvalidArgument
        );

        let buttonless = Call::ClientPointer {
            client: ClientId::new(1),
            x: 0,
            y: 0,
            action: PointerAction::Click,
            button: None,
            ensure_visible: false,
            expects: Expects::default(),
        };
        assert_eq!(
            buttonless.validate().expect_err("needs button").code,
            ErrorCode::InvalidArgument
        );

        Call::ClientPointer {
            client: ClientId::new(1),
            x: 0,
            y: 0,
            action: PointerAction::Move,
            button: None,
            ensure_visible: false,
            expects: Expects::default(),
        }
        .validate()
        .expect("move needs no button");
    }

    #[test]
    fn control_events_cannot_be_filtered_away() {
        assert!(!EventKind::SessionControl.is_filterable());
        assert!(!EventKind::ResyncRequired.is_filterable());
        assert!(EventKind::TitleChanged.is_filterable());
    }

    #[test]
    fn events_report_their_own_kind() {
        let event = Event::TitleChanged {
            client: ClientId::new(2),
            generation: Generation::new(4),
            title: Some("editor".to_owned()),
        };
        assert_eq!(event.kind(), EventKind::TitleChanged);
        assert_eq!(round_trip(&event), event);
        let value = serde_json::to_value(&event).expect("encodes");
        assert_eq!(value["event"], "title_changed");
    }

    #[test]
    fn human_activity_carries_no_content() {
        let event = Event::HumanActivity {
            kind: super::HumanActivityKind::Keyboard,
        };
        let encoded = serde_json::to_string(&event).expect("encodes");
        assert_eq!(
            encoded,
            "{\"event\":\"human_activity\",\"kind\":\"keyboard\"}"
        );
    }

    #[test]
    fn envelopes_round_trip_in_both_directions() {
        let request = ClientMessage::Request(Request {
            id: RequestId::new(1),
            call: Call::DesktopSnapshot {},
        });
        assert_eq!(round_trip(&request), request);

        let denied = ServerMessage::Response(Response {
            id: RequestId::new(1),
            sequence: Sequence::new(12),
            outcome: Outcome::Error {
                error: ProtocolError::denied("session holds no capabilities"),
            },
        });
        assert_eq!(round_trip(&denied), denied);
        let ServerMessage::Response(response) = &denied else {
            panic!("wrong variant");
        };
        assert_eq!(response.outcome.code(), Some(ErrorCode::Denied));
        assert!(!response.outcome.is_ok());
    }

    #[test]
    fn committed_steps_survive_the_wire() {
        let outcome = Outcome::Ok {
            reply: super::Reply::Committed {
                committed: vec![Step::WorkspaceSwitch, Step::Activate, Step::Inject],
                sequence: Sequence::new(90),
            },
        };
        assert_eq!(round_trip(&outcome), outcome);
    }
}
