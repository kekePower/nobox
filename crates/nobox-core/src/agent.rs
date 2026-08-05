//! Agent-seat policy.
//!
//! Everything an agent session is allowed to know or do is decided here:
//! capability evaluation, application scope, sensitive-client visibility,
//! snapshot assembly, and the counters that make a pushed world model
//! trustworthy. This module is pure and deterministic — no I/O, no frames, no
//! display-server types — so the rules that matter for security are the ones
//! that are easiest to test.
//!
//! Two invariants shape the code. A hidden client is indistinguishable from a
//! client that never existed, in every answer and every error. And no
//! capability implies another: a session holds exactly the atoms it was
//! granted, and each request is checked against all of the atoms it needs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use agent_seat_proto as proto;

use crate::{
    Client, ClientId, ClientSet, Geometry, OutputId, OutputSet, WorkspaceAssignment, WorkspaceId,
};

/// How visible a client is to agent sessions.
///
/// Ordered by sensitivity, because visibility only ever ratchets toward the
/// more private value: see [`AgentState::observe_client`].
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum AgentVisibility {
    /// Subject only to the session's grant and scope.
    #[default]
    Visible,
    /// Present with geometry but no title; capture and input are refused.
    Redacted,
    /// Absent from every response and event.
    Hidden,
}

/// Facts only a display-server backend knows about a client.
///
/// The backend supplies these; this module decides what a session may see of
/// them.
pub trait ClientDetails {
    /// Returns the client's application identity.
    fn application(&self, client: ClientId) -> proto::ApplicationIdentity;

    /// Returns the client's current title, if it has one.
    fn title(&self, client: ClientId) -> Option<String>;

    /// Returns the client's decorated frame rectangle.
    fn frame(&self, client: ClientId) -> Geometry;

    /// Returns a workspace's configured name, if it has one.
    fn workspace_name(&self, workspace: WorkspaceId) -> Option<String>;

    /// Returns an output's backend name, if it has one.
    fn output_name(&self, output: OutputId) -> Option<String>;

    /// Returns an output's rectangle after panels reserve space.
    fn work_area(&self, output: OutputId) -> Geometry;
}

/// What one session was granted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Grant {
    capabilities: proto::CapabilitySet,
    scope: Option<BTreeSet<ClientId>>,
}

impl Grant {
    /// Builds an unscoped grant.
    #[must_use]
    pub const fn new(capabilities: proto::CapabilitySet) -> Self {
        Self {
            capabilities,
            scope: None,
        }
    }

    /// Builds a grant restricted to an application scope. Membership is
    /// decided by the backend when a client is managed, so scope evaluation
    /// and rule matching stay in one place.
    #[must_use]
    pub fn scoped(capabilities: proto::CapabilitySet) -> Self {
        Self {
            capabilities,
            scope: Some(BTreeSet::new()),
        }
    }

    /// The empty grant: a live session that may do nothing.
    #[must_use]
    pub const fn denied() -> Self {
        Self::new(proto::CapabilitySet::EMPTY)
    }

    /// Returns the atoms held.
    #[must_use]
    pub const fn capabilities(&self) -> proto::CapabilitySet {
        self.capabilities
    }

    /// Returns whether an application scope restricts this grant.
    #[must_use]
    pub const fn is_scoped(&self) -> bool {
        self.scope.is_some()
    }

    /// Returns whether `client` is inside the scope.
    #[must_use]
    pub fn covers(&self, client: ClientId) -> bool {
        self.scope
            .as_ref()
            .is_none_or(|scope| scope.contains(&client))
    }

    /// Records that `client` matches this grant's scope.
    pub fn include(&mut self, client: ClientId) {
        if let Some(scope) = self.scope.as_mut() {
            scope.insert(client);
        }
    }

    fn forget(&mut self, client: ClientId) {
        if let Some(scope) = self.scope.as_mut() {
            scope.remove(&client);
        }
    }
}

/// Whether a session may still act.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionStatus {
    /// Ordinary.
    #[default]
    Active,
    /// Frozen by the human. Freezing is not revocation: the grant survives,
    /// and the human decides afterward whether the session resumes.
    Frozen,
    /// The grant was withdrawn.
    Revoked,
}

/// Events one session may have queued before it is told to start over.
///
/// A slow consumer degrades itself: its backlog is dropped and it is asked to
/// re-snapshot, which costs it a round trip and costs the manager nothing.
pub const MAX_QUEUED_EVENTS: usize = 512;

/// One session's event stream.
#[derive(Clone, Debug, Default)]
struct Subscription {
    /// Kinds to deliver. Empty means every kind.
    kinds: BTreeSet<proto::EventKind>,
    queue: VecDeque<proto::EventEnvelope>,
    /// Events dropped since the agent was last told to re-snapshot.
    dropped: u64,
    /// Whether the agent still needs to be told its world model is invalid.
    resync: bool,
}

impl Subscription {
    fn wants(&self, kind: proto::EventKind) -> bool {
        // Unfilterable kinds are delivered whatever the filter says: an agent
        // must never be able to filter away the news that its world model is
        // invalid.
        !kind.is_filterable() || self.kinds.is_empty() || self.kinds.contains(&kind)
    }
}

/// One agent session's policy state.
#[derive(Clone, Debug)]
pub struct AgentSession {
    grant: Grant,
    status: SessionStatus,
    subscription: Option<Subscription>,
    /// This session's own view of how far the desktop has moved.
    ///
    /// Per session rather than global, for two reasons. A shared counter moves
    /// when another session is delivered an event, so its value depends on who
    /// else happens to be watching — and a session scoped to one application
    /// could read activity outside its scope out of the jumps, which is
    /// exactly what scoping promises cannot happen. Absolute ordering across
    /// sessions bought nothing in return: no agent can observe another
    /// session's events, so no agent could ever compare the two.
    sequence: proto::Sequence,
}

impl AgentSession {
    /// Returns the session's grant.
    #[must_use]
    pub const fn grant(&self) -> &Grant {
        &self.grant
    }

    /// Returns the session's status.
    #[must_use]
    pub const fn status(&self) -> SessionStatus {
        self.status
    }

    /// Returns whether the session is receiving events.
    #[must_use]
    pub const fn is_subscribed(&self) -> bool {
        self.subscription.is_some()
    }
}

/// Every agent session, and the counters their world models depend on.
#[derive(Clone, Debug, Default)]
pub struct AgentState {
    sessions: BTreeMap<proto::SessionId, AgentSession>,
    visibility: BTreeMap<ClientId, AgentVisibility>,
    generations: BTreeMap<ClientId, proto::Generation>,
}

impl AgentState {
    /// Returns an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns one session's current sequence number.
    ///
    /// Every snapshot, capture, and event this session is given is stamped
    /// from this counter, so the session can order them absolutely against
    /// each other. It counts desktop changes this session could observe, and
    /// nothing else: a client repainting, a page loading, or a document
    /// rendering moves no window and therefore does not move this.
    #[must_use]
    pub fn sequence(&self, session: proto::SessionId) -> proto::Sequence {
        self.sessions
            .get(&session)
            .map_or(proto::Sequence::ZERO, |state| state.sequence)
    }

    /// Opens a session with an already-decided grant.
    pub fn open(&mut self, session: proto::SessionId, grant: Grant) {
        self.sessions.insert(
            session,
            AgentSession {
                grant,
                status: SessionStatus::Active,
                subscription: None,
                sequence: proto::Sequence::ZERO,
            },
        );
    }

    /// Replaces a session's grant without disturbing its stream.
    ///
    /// Re-evaluating configuration must not cost a session the subscription it
    /// established, or the event telling it what just happened would be the
    /// first one it never receives.
    pub fn set_grant(&mut self, session: proto::SessionId, grant: Grant) -> bool {
        let Some(state) = self.sessions.get_mut(&session) else {
            return false;
        };
        state.grant = grant;
        true
    }

    /// Ends a session.
    pub fn close(&mut self, session: proto::SessionId) -> bool {
        self.sessions.remove(&session).is_some()
    }

    /// Returns a session.
    #[must_use]
    pub fn session(&self, session: proto::SessionId) -> Option<&AgentSession> {
        self.sessions.get(&session)
    }

    /// Returns every open session.
    pub fn sessions(&self) -> impl Iterator<Item = (proto::SessionId, &AgentSession)> {
        self.sessions.iter().map(|(id, state)| (*id, state))
    }

    /// Returns whether any session is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Freezes every session, returning those whose status changed.
    ///
    /// Freezing is not revocation: the grant survives, and the human decides
    /// afterward whether the session resumes or ends.
    pub fn freeze_all(&mut self) -> Vec<proto::SessionId> {
        self.set_all(SessionStatus::Frozen)
    }

    /// Resumes every frozen session, returning those whose status changed.
    pub fn resume_all(&mut self) -> Vec<proto::SessionId> {
        self.sessions
            .iter_mut()
            .filter(|(_, state)| state.status == SessionStatus::Frozen)
            .map(|(id, state)| {
                state.status = SessionStatus::Active;
                *id
            })
            .collect()
    }

    /// Revokes every grant, returning the sessions that held one.
    pub fn revoke_all(&mut self) -> Vec<proto::SessionId> {
        self.set_all(SessionStatus::Revoked)
    }

    /// Returns whether any session is frozen.
    #[must_use]
    pub fn any_frozen(&self) -> bool {
        self.sessions
            .values()
            .any(|state| state.status == SessionStatus::Frozen)
    }

    fn set_all(&mut self, status: SessionStatus) -> Vec<proto::SessionId> {
        self.sessions
            .iter_mut()
            .filter(|(_, state)| state.status != status)
            .map(|(id, state)| {
                state.status = status;
                *id
            })
            .collect()
    }

    /// Returns whether any session holds a capability that must be shown to
    /// the human while it is held.
    #[must_use]
    pub fn any_holds_visible_capability(&self) -> bool {
        self.sessions.values().any(|state| {
            [
                proto::Capability::InputPointer,
                proto::Capability::InputKeyboard,
                proto::Capability::CaptureClientVisible,
                proto::Capability::CaptureClientObscured,
                proto::Capability::CaptureOutput,
            ]
            .into_iter()
            .any(|atom| state.grant.capabilities.holds(atom))
        })
    }

    /// Sets a session's status.
    pub fn set_status(&mut self, session: proto::SessionId, status: SessionStatus) -> bool {
        self.sessions
            .get_mut(&session)
            .map(|state| state.status = status)
            .is_some()
    }

    /// Records a client's configured visibility and its scope membership in
    /// every session whose grant is scoped to it.
    ///
    /// Visibility only ratchets toward the more private value. Application
    /// rules match on identity, and a client controls part of its own identity
    /// — most obviously its title — so re-evaluating a rule could otherwise
    /// let a window that was hidden when it appeared rename itself back into
    /// view. A client that becomes sensitive is hidden; one that was sensitive
    /// stays hidden for as long as it is managed.
    pub fn observe_client(
        &mut self,
        client: ClientId,
        visibility: AgentVisibility,
        mut in_scope: impl FnMut(proto::SessionId) -> bool,
    ) {
        self.visibility
            .entry(client)
            .and_modify(|current| *current = (*current).max(visibility))
            .or_insert(visibility);
        self.generations
            .entry(client)
            .or_insert(proto::Generation::FIRST);
        for (id, state) in &mut self.sessions {
            if in_scope(*id) {
                state.grant.include(client);
            }
        }
    }

    /// Forgets a client that is no longer managed.
    pub fn forget_client(&mut self, client: ClientId) {
        self.visibility.remove(&client);
        self.generations.remove(&client);
        for state in self.sessions.values_mut() {
            state.grant.forget(client);
        }
    }

    /// Returns a client's configured visibility.
    #[must_use]
    pub fn visibility(&self, client: ClientId) -> AgentVisibility {
        self.visibility.get(&client).copied().unwrap_or_default()
    }

    /// Returns a client's current generation.
    #[must_use]
    pub fn generation(&self, client: ClientId) -> proto::Generation {
        self.generations
            .get(&client)
            .copied()
            .unwrap_or(proto::Generation::FIRST)
    }

    /// Bumps a client's generation, returning the new value.
    ///
    /// Generations move per client; the sequence moves when something is
    /// published. Keeping them separate is what lets a freshness check say
    /// "this client, as I last saw it" without global-sequence equality.
    pub fn touch(&mut self, client: ClientId) -> proto::Generation {
        let generation = self
            .generations
            .entry(client)
            .and_modify(|generation| *generation = generation.next())
            .or_insert(proto::Generation::FIRST);
        *generation
    }

    /// Returns whether a session may perceive a client at all.
    ///
    /// Hidden clients and out-of-scope clients are equally absent. Callers
    /// must answer requests naming them exactly as they answer requests
    /// naming a client that never existed.
    #[must_use]
    pub fn perceives(&self, session: proto::SessionId, client: ClientId) -> bool {
        let Some(state) = self.sessions.get(&session) else {
            return false;
        };
        if matches!(self.visibility(client), AgentVisibility::Hidden) {
            return false;
        }
        state.grant.covers(client)
    }

    /// Checks a call against a session's grant and status.
    ///
    /// # Errors
    ///
    /// Returns the structured refusal to send back: the session is gone,
    /// frozen, revoked, or missing at least one required capability.
    pub fn authorize(
        &self,
        session: proto::SessionId,
        call: &proto::Call,
    ) -> Result<(), proto::ProtocolError> {
        let Some(state) = self.sessions.get(&session) else {
            return Err(proto::ProtocolError::new(
                proto::ErrorCode::SessionRevoked,
                "this session is closed",
            ));
        };
        match state.status {
            SessionStatus::Active => {}
            SessionStatus::Frozen => {
                return Err(proto::ProtocolError::new(
                    proto::ErrorCode::SessionFrozen,
                    "this session is frozen",
                ));
            }
            SessionStatus::Revoked => {
                return Err(proto::ProtocolError::new(
                    proto::ErrorCode::SessionRevoked,
                    "this session's grant was revoked",
                ));
            }
        }
        let required = call.required();
        if required.intersection(state.grant.capabilities) == required {
            Ok(())
        } else {
            Err(proto::ProtocolError::denied(
                "this session was not granted that capability",
            ))
        }
    }

    /// Begins delivering events to a session, replacing any previous
    /// subscription and its backlog.
    ///
    /// The caller takes the snapshot in the same event-loop boundary, so the
    /// snapshot and the stream it continues from are established as one
    /// operation and no event can fall between them.
    pub fn subscribe(&mut self, session: proto::SessionId, kinds: &[proto::EventKind]) -> bool {
        let Some(state) = self.sessions.get_mut(&session) else {
            return false;
        };
        state.subscription = Some(Subscription {
            kinds: kinds.iter().copied().collect(),
            ..Subscription::default()
        });
        true
    }

    /// Returns the sessions that should receive an event of `kind` about
    /// `subject`, in session order.
    ///
    /// Scope and sensitive-client visibility filter events exactly as they
    /// filter snapshots: a session that cannot perceive a client is never told
    /// anything about it.
    #[must_use]
    pub fn subscribers(
        &self,
        kind: proto::EventKind,
        subject: Option<ClientId>,
    ) -> Vec<proto::SessionId> {
        self.sessions
            .iter()
            .filter(|(id, state)| {
                state
                    .subscription
                    .as_ref()
                    .is_some_and(|subscription| subscription.wants(kind))
                    && subject.is_none_or(|client| self.perceives(**id, client))
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Returns every session that could have observed a change, whether or not
    /// it asked to be told about one.
    ///
    /// This is what moves a session's sequence. Subscribing is how an agent
    /// receives events; it is not what makes the desktop change. A session
    /// that never subscribes still watched the desktop move underneath it, and
    /// a counter that stayed at zero through all of it — which is what a
    /// delivery-driven counter did — reads as broken rather than as quiet.
    #[must_use]
    pub fn observers(&self, subject: Option<ClientId>) -> Vec<proto::SessionId> {
        self.sessions
            .keys()
            .copied()
            .filter(|id| subject.is_none_or(|client| self.perceives(*id, client)))
            .collect()
    }

    /// Advances the sequence of every session that could observe a change, and
    /// returns nothing: the value a caller wants is always one session's.
    pub fn touch_observers(&mut self, subject: Option<ClientId>) {
        for id in self.observers(subject) {
            if let Some(state) = self.sessions.get_mut(&id) {
                state.sequence = state.sequence.next();
            }
        }
    }

    /// Stamps and queues already-built, per-session events.
    ///
    /// Each event is stamped with its own session's sequence, which
    /// [`AgentState::touch_observers`] has already advanced for this change.
    pub fn publish(&mut self, events: impl IntoIterator<Item = (proto::SessionId, proto::Event)>) {
        for (session, event) in events {
            let Some(state) = self.sessions.get_mut(&session) else {
                continue;
            };
            let sequence = state.sequence;
            let Some(subscription) = state.subscription.as_mut() else {
                continue;
            };
            if subscription.queue.len() >= MAX_QUEUED_EVENTS {
                // The backlog is worthless once it is incomplete; drop it and
                // ask for a fresh world model instead of delivering a gap.
                subscription.dropped = subscription
                    .dropped
                    .saturating_add(subscription.queue.len() as u64 + 1);
                subscription.queue.clear();
                subscription.resync = true;
                continue;
            }
            if subscription.resync {
                subscription.dropped = subscription.dropped.saturating_add(1);
                continue;
            }
            subscription
                .queue
                .push_back(proto::EventEnvelope { sequence, event });
        }
    }

    /// Takes what a session should be sent now.
    ///
    /// After an overflow this is exactly one `resync_required`: the agent must
    /// re-snapshot, and anything queued behind the gap would be misleading.
    pub fn take_events(&mut self, session: proto::SessionId) -> Vec<proto::EventEnvelope> {
        let Some(state) = self.sessions.get_mut(&session) else {
            return Vec::new();
        };
        let sequence = state.sequence;
        let Some(subscription) = state.subscription.as_mut() else {
            return Vec::new();
        };
        if subscription.resync {
            let dropped = subscription.dropped;
            subscription.resync = false;
            subscription.dropped = 0;
            subscription.queue.clear();
            return vec![proto::EventEnvelope {
                sequence,
                event: proto::Event::ResyncRequired { dropped },
            }];
        }
        subscription.queue.drain(..).collect()
    }

    /// Takes the next event for a session, if any.
    ///
    /// Delivery is one at a time so a caller whose transport is momentarily
    /// full can hand an event back rather than drop it.
    pub fn pop_event(&mut self, session: proto::SessionId) -> Option<proto::EventEnvelope> {
        let sequence = self.sequence(session);
        let subscription = self.sessions.get_mut(&session)?.subscription.as_mut()?;
        if subscription.resync {
            let dropped = subscription.dropped;
            subscription.resync = false;
            subscription.dropped = 0;
            subscription.queue.clear();
            return Some(proto::EventEnvelope {
                sequence,
                event: proto::Event::ResyncRequired { dropped },
            });
        }
        subscription.queue.pop_front()
    }

    /// Returns an undelivered event to the front of its session's queue.
    pub fn requeue_event(&mut self, session: proto::SessionId, envelope: proto::EventEnvelope) {
        let Some(subscription) = self
            .sessions
            .get_mut(&session)
            .and_then(|state| state.subscription.as_mut())
        else {
            return;
        };
        if matches!(envelope.event, proto::Event::ResyncRequired { dropped }
            if { subscription.resync = true; subscription.dropped = dropped; true })
        {
            return;
        }
        subscription.queue.push_front(envelope);
    }

    /// Returns whether a session has anything to deliver.
    #[must_use]
    pub fn has_events(&self, session: proto::SessionId) -> bool {
        self.sessions.get(&session).is_some_and(|state| {
            state
                .subscription
                .as_ref()
                .is_some_and(|subscription| subscription.resync || !subscription.queue.is_empty())
        })
    }

    /// Returns whether any session is receiving events.
    #[must_use]
    pub fn any_subscribed(&self) -> bool {
        self.sessions
            .values()
            .any(|state| state.subscription.is_some())
    }

    /// Checks the state an agent believes it is acting on.
    ///
    /// An agent that says "click this window only if it is still what I
    /// inspected" gets exactly that: the manager refuses rather than acting on
    /// an obsolete assumption, and the refusal names the current generation so
    /// re-observing costs one round trip.
    ///
    /// # Errors
    ///
    /// Returns [`proto::ErrorCode::StaleState`] when any stated precondition
    /// no longer holds.
    pub fn check_expects(
        &self,
        client: ClientId,
        expects: &proto::Expects,
        clients: &ClientSet,
    ) -> Result<(), proto::ProtocolError> {
        if expects.is_empty() {
            return Ok(());
        }
        let generation = self.generation(client);
        let stale = || proto::ProtocolError::stale_state(generation);
        let Some(managed) = clients.get(client) else {
            return Err(proto::ProtocolError::no_such_client());
        };
        if expects
            .generation
            .is_some_and(|expected| expected != generation)
        {
            return Err(stale());
        }
        if expects
            .content
            .is_some_and(|expected| expected != rect(managed.geometry))
        {
            return Err(stale());
        }
        if let Some(expected) = expects.workspace {
            let actual = match managed.workspace {
                WorkspaceAssignment::All => None,
                WorkspaceAssignment::Workspace(workspace) => {
                    Some(proto::WorkspaceId::new(workspace.index()))
                }
            };
            if actual != Some(expected) {
                return Err(stale());
            }
        }
        if expects
            .focused
            .is_some_and(|expected| expected != (clients.focused() == Some(client)))
        {
            return Err(stale());
        }
        Ok(())
    }

    /// Builds one client descriptor as `session` may see it, or `None` when
    /// the session cannot perceive the client.
    #[must_use]
    pub fn descriptor(
        &self,
        session: proto::SessionId,
        client: ClientId,
        clients: &ClientSet,
        outputs: &OutputSet,
        details: &impl ClientDetails,
    ) -> Option<proto::ClientDescriptor> {
        if !self.perceives(session, client) {
            return None;
        }
        let state = self.sessions.get(&session)?;
        let managed = clients.get(client)?;
        let redacted = matches!(self.visibility(client), AgentVisibility::Redacted);
        let titles = state
            .grant
            .capabilities
            .holds(proto::Capability::ObserveTitles);
        let title = if redacted || !titles {
            None
        } else {
            details.title(client)
        };
        let geometry = managed.geometry;
        Some(proto::ClientDescriptor {
            client: proto::ClientId::new(client.raw()),
            generation: self.generation(client),
            application: details.application(client),
            title,
            redacted,
            content: rect(geometry),
            frame: rect(details.frame(client)),
            workspace: match managed.workspace {
                WorkspaceAssignment::All => None,
                WorkspaceAssignment::Workspace(workspace) => {
                    Some(proto::WorkspaceId::new(workspace.index()))
                }
            },
            output: Some(proto::OutputId::new(outputs.output_for(geometry).id.raw())),
            state: client_state_of(clients, managed),
            transient_for: match managed.transient_for {
                Some(crate::TransientTarget::Client(parent)) => {
                    Some(proto::ClientId::new(parent.raw()))
                }
                Some(crate::TransientTarget::Group) | None => None,
            },
        })
    }

    /// Builds the whole world model as `session` may see it.
    #[must_use]
    pub fn snapshot(
        &self,
        session: proto::SessionId,
        clients: &ClientSet,
        outputs: &OutputSet,
        details: &impl ClientDetails,
    ) -> proto::DesktopSnapshot {
        let descriptors: Vec<proto::ClientDescriptor> = clients
            .management_order()
            .filter_map(|client| self.descriptor(session, client, clients, outputs, details))
            .collect();
        let perceived: BTreeSet<ClientId> = clients
            .management_order()
            .filter(|client| self.perceives(session, *client))
            .collect();
        let stacking = clients
            .stacking()
            .filter(|client| perceived.contains(client))
            .map(|client| proto::ClientId::new(client.raw()))
            .collect();
        let focused = clients
            .focused()
            .filter(|client| perceived.contains(client))
            .map(|client| proto::ClientId::new(client.raw()));
        proto::DesktopSnapshot {
            sequence: self.sequence(session),
            outputs: outputs
                .outputs()
                .iter()
                .map(|output| proto::OutputDescriptor {
                    output: proto::OutputId::new(output.id.raw()),
                    name: details.output_name(output.id),
                    geometry: rect(output.geometry),
                    work_area: rect(details.work_area(output.id)),
                    primary: output.primary,
                })
                .collect(),
            workspaces: (0..clients.workspace_count())
                .map(|index| proto::WorkspaceDescriptor {
                    workspace: proto::WorkspaceId::new(index),
                    name: details.workspace_name(WorkspaceId::new(index)),
                })
                .collect(),
            current_workspace: proto::WorkspaceId::new(clients.current_workspace().index()),
            focused,
            stacking,
            clients: descriptors,
        }
    }
}

/// Converts a policy rectangle to its protocol form.
#[must_use]
pub const fn rect(geometry: Geometry) -> proto::Rect {
    proto::Rect::new(geometry.x, geometry.y, geometry.width, geometry.height)
}

/// Returns whether agent input is suppressed because the human just acted.
///
/// The human wins structurally: any human input opens a window during which
/// agent input is refused, and the protocol offers no way to contend for the
/// pointer or keyboard. Politeness is not delegated to the agent.
#[must_use]
pub fn is_suppressed(since_human_input: Option<Duration>, window: Duration) -> bool {
    if window.is_zero() {
        return false;
    }
    since_human_input.is_some_and(|elapsed| elapsed < window)
}

/// Builds a client's protocol state flags.
#[must_use]
pub fn client_state(clients: &ClientSet, client: ClientId) -> Option<proto::ClientState> {
    clients
        .get(client)
        .map(|managed| client_state_of(clients, managed))
}

fn client_state_of(clients: &ClientSet, client: &Client) -> proto::ClientState {
    let (horizontal, vertical) = client.maximize.map_or((false, false), |maximize| {
        (maximize.horizontal, maximize.vertical)
    });
    proto::ClientState {
        focused: clients.focused() == Some(client.id),
        visible: clients.is_visible(client.id),
        minimized: client.iconic,
        maximized_horizontal: horizontal,
        maximized_vertical: vertical,
        fullscreen: client.fullscreen.is_some(),
        shaded: client.shaded,
        sticky: matches!(client.workspace, WorkspaceAssignment::All),
        above: matches!(client.layer, crate::ClientLayer::Above),
        below: matches!(client.layer, crate::ClientLayer::Below),
        urgent: client.presentation.urgent,
        modal: client.modal,
        decorated: client.policy.decorations.is_present(),
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentState, AgentVisibility, ClientDetails, Grant};
    use crate::{
        Client, ClientId, ClientPolicy, ClientRole, ClientSet, Geometry, Output, OutputId,
        OutputSet, WorkspaceAssignment, WorkspaceId,
    };
    use agent_seat_proto as proto;

    struct Details;

    impl ClientDetails for Details {
        fn application(&self, client: ClientId) -> proto::ApplicationIdentity {
            proto::ApplicationIdentity {
                class: Some(format!("Class{}", client.raw())),
                ..proto::ApplicationIdentity::default()
            }
        }

        fn title(&self, client: ClientId) -> Option<String> {
            Some(format!("title {}", client.raw()))
        }

        fn frame(&self, _client: ClientId) -> Geometry {
            Geometry::new(0, 0, 120, 140)
        }

        fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
            Some(format!("workspace {}", workspace.index() + 1))
        }

        fn output_name(&self, _output: OutputId) -> Option<String> {
            Some("HEAD-0".to_owned())
        }

        fn work_area(&self, _output: OutputId) -> Geometry {
            Geometry::new(0, 20, 800, 580)
        }
    }

    fn client(id: u64) -> Client {
        Client {
            id: ClientId::new(id),
            geometry: Geometry::new(10, 10, 100, 120),
            size_hints: crate::SizeHints::default(),
            gravity: crate::Gravity::default(),
            policy: ClientPolicy::for_role(ClientRole::Normal),
            natural_decorations: ClientPolicy::for_role(ClientRole::Normal).decorations,
            decoration_override: crate::DecorationOverride::Default,
            presentation: crate::ClientPresentation::default(),
            transient_for: None,
            group: None,
            modal: false,
            iconic: false,
            shaded: false,
            workspace: WorkspaceAssignment::Workspace(WorkspaceId::new(0)),
            layer: crate::ClientLayer::Normal,
            maximize: None,
            fullscreen: None,
            output_coverage: None,
        }
    }

    fn desktop() -> (ClientSet, OutputSet) {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(4);
        clients.manage(client(1));
        clients.manage(client(2));
        clients.manage(client(3));
        let outputs = OutputSet::new([Output {
            id: OutputId::new(0),
            geometry: Geometry::new(0, 0, 800, 600),
            primary: true,
        }]);
        (clients, outputs)
    }

    fn session(state: &mut AgentState, grant: Grant) -> proto::SessionId {
        let id = proto::SessionId::new(1);
        state.open(id, grant);
        id
    }

    fn observe_all(state: &mut AgentState, clients: &ClientSet, visible: AgentVisibility) {
        for client in clients.management_order().collect::<Vec<_>>() {
            state.observe_client(client, visible, |_| true);
        }
    }

    #[test]
    fn a_hidden_client_is_absent_from_snapshots_and_from_perception() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::from_iter_atoms([
                proto::Capability::ObserveStructure,
                proto::Capability::ObserveTitles,
            ])),
        );
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        state.observe_client(ClientId::new(2), AgentVisibility::Hidden, |_| true);

        let snapshot = state.snapshot(id, &clients, &outputs, &Details);
        let seen: Vec<u64> = snapshot
            .clients
            .iter()
            .map(|descriptor| descriptor.client.raw())
            .collect();
        assert_eq!(seen, vec![1, 3]);
        assert!(!snapshot.stacking.contains(&proto::ClientId::new(2)));
        assert!(!state.perceives(id, ClientId::new(2)));
        assert!(
            state
                .descriptor(id, ClientId::new(2), &clients, &outputs, &Details)
                .is_none()
        );
    }

    #[test]
    fn a_redacted_client_keeps_its_geometry_and_loses_its_title() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::from_iter_atoms([
                proto::Capability::ObserveStructure,
                proto::Capability::ObserveTitles,
            ])),
        );
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        state.observe_client(ClientId::new(2), AgentVisibility::Redacted, |_| true);

        let descriptor = state
            .descriptor(id, ClientId::new(2), &clients, &outputs, &Details)
            .expect("present");
        assert!(descriptor.redacted);
        assert_eq!(descriptor.title, None);
        assert_eq!(descriptor.content, proto::Rect::new(10, 10, 100, 120));
    }

    #[test]
    fn titles_require_their_own_atom() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        let descriptor = state
            .descriptor(id, ClientId::new(1), &clients, &outputs, &Details)
            .expect("present");
        assert_eq!(descriptor.title, None);
        assert!(!descriptor.redacted, "withholding a title is not redaction");
    }

    #[test]
    fn scope_makes_other_clients_absent_rather_than_inert() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::scoped(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        for client in clients.management_order().collect::<Vec<_>>() {
            let matches = client == ClientId::new(3);
            state.observe_client(client, AgentVisibility::Visible, |_| matches);
        }
        let snapshot = state.snapshot(id, &clients, &outputs, &Details);
        let seen: Vec<u64> = snapshot
            .clients
            .iter()
            .map(|descriptor| descriptor.client.raw())
            .collect();
        assert_eq!(seen, vec![3]);
        assert!(!state.perceives(id, ClientId::new(1)));
        assert_eq!(snapshot.stacking, vec![proto::ClientId::new(3)]);
    }

    #[test]
    fn focus_outside_the_scope_is_not_reported() {
        let (mut clients, outputs) = desktop();
        clients.focus(ClientId::new(1));
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::scoped(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        for client in clients.management_order().collect::<Vec<_>>() {
            let matches = client == ClientId::new(3);
            state.observe_client(client, AgentVisibility::Visible, |_| matches);
        }
        let snapshot = state.snapshot(id, &clients, &outputs, &Details);
        assert_eq!(snapshot.focused, None);
    }

    #[test]
    fn authorization_requires_every_atom_a_call_needs() {
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        state
            .authorize(id, &proto::Call::DesktopSnapshot {})
            .expect("observe is granted");
        let denied = state
            .authorize(
                id,
                &proto::Call::WorkspaceSwitch {
                    workspace: proto::WorkspaceId::new(1),
                },
            )
            .expect_err("manage is not granted");
        assert_eq!(denied.code, proto::ErrorCode::Denied);
    }

    #[test]
    fn frozen_and_revoked_sessions_are_distinguishable_and_both_refuse() {
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        state.set_status(id, super::SessionStatus::Frozen);
        assert_eq!(
            state
                .authorize(id, &proto::Call::DesktopSnapshot {})
                .expect_err("frozen")
                .code,
            proto::ErrorCode::SessionFrozen
        );
        state.set_status(id, super::SessionStatus::Revoked);
        assert_eq!(
            state
                .authorize(id, &proto::Call::DesktopSnapshot {})
                .expect_err("revoked")
                .code,
            proto::ErrorCode::SessionRevoked
        );
        state.close(id);
        assert_eq!(
            state
                .authorize(id, &proto::Call::DesktopSnapshot {})
                .expect_err("closed")
                .code,
            proto::ErrorCode::SessionRevoked
        );
    }

    #[test]
    fn generations_and_sequences_advance_independently_per_client() {
        let mut state = AgentState::new();
        state.observe_client(ClientId::new(1), AgentVisibility::Visible, |_| true);
        state.observe_client(ClientId::new(2), AgentVisibility::Visible, |_| true);
        assert_eq!(state.generation(ClientId::new(1)), proto::Generation::FIRST);

        assert_eq!(state.touch(ClientId::new(1)), proto::Generation::new(2));
        assert_eq!(state.generation(ClientId::new(2)), proto::Generation::FIRST);
        assert_eq!(state.touch(ClientId::new(1)), proto::Generation::new(3));
        assert_eq!(
            state.sequence(proto::SessionId::new(1)),
            proto::Sequence::ZERO,
            "generations move without touching any session's sequence"
        );
    }

    #[test]
    fn a_hidden_client_cannot_rename_itself_back_into_view() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.observe_client(ClientId::new(2), AgentVisibility::Hidden, |_| true);
        // A later rule evaluation, against an identity the client controls.
        state.observe_client(ClientId::new(2), AgentVisibility::Visible, |_| true);
        assert_eq!(state.visibility(ClientId::new(2)), AgentVisibility::Hidden);
        assert!(!state.perceives(id, ClientId::new(2)));
        assert!(
            state
                .descriptor(id, ClientId::new(2), &clients, &outputs, &Details)
                .is_none()
        );

        // Becoming sensitive still takes effect immediately.
        state.observe_client(ClientId::new(1), AgentVisibility::Visible, |_| true);
        state.observe_client(ClientId::new(1), AgentVisibility::Redacted, |_| true);
        assert_eq!(
            state.visibility(ClientId::new(1)),
            AgentVisibility::Redacted
        );

        // Only ending management clears it.
        state.forget_client(ClientId::new(2));
        state.observe_client(ClientId::new(2), AgentVisibility::Visible, |_| true);
        assert_eq!(state.visibility(ClientId::new(2)), AgentVisibility::Visible);
    }

    #[test]
    fn forgetting_a_client_clears_its_visibility_and_scope_membership() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::scoped(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        state.observe_client(ClientId::new(1), AgentVisibility::Hidden, |_| true);
        assert!(!state.perceives(id, ClientId::new(1)));
        state.forget_client(ClientId::new(1));
        assert_eq!(state.visibility(ClientId::new(1)), AgentVisibility::Visible);
        assert!(
            !state.perceives(id, ClientId::new(1)),
            "a forgotten client is outside every scope"
        );
        assert!(
            state
                .descriptor(id, ClientId::new(1), &clients, &outputs, &Details)
                .is_none()
        );
    }

    fn observing_session(state: &mut AgentState, clients: &ClientSet) -> proto::SessionId {
        let id = session(
            state,
            Grant::new(proto::CapabilitySet::from_iter_atoms([
                proto::Capability::ObserveStructure,
                proto::Capability::ObserveTitles,
            ])),
        );
        observe_all(state, clients, AgentVisibility::Visible);
        id
    }

    fn focus_event(client: u64) -> proto::Event {
        proto::Event::FocusChanged {
            client: Some(proto::ClientId::new(client)),
        }
    }

    #[test]
    fn events_reach_only_subscribed_sessions() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.publish([(id, focus_event(1))]);
        assert!(
            !state.has_events(id),
            "an unsubscribed session queues nothing"
        );

        assert!(state.subscribe(id, &[]));
        state.touch_observers(None);
        let sequence = state.sequence(id);
        state.publish([(id, focus_event(1))]);
        let delivered = state.take_events(id);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].sequence, sequence);
        assert_eq!(delivered[0].event, focus_event(1));
        assert!(state.take_events(id).is_empty(), "events deliver once");
    }

    #[test]
    fn a_sequence_counts_what_its_own_session_could_observe() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let first = observing_session(&mut state, &clients);
        let second = proto::SessionId::new(2);
        state.open(second, Grant::new(proto::CapabilitySet::EMPTY));
        state.subscribe(first, &[]);

        // Both sessions could see this change, so both counters move — even
        // the one that never subscribed. A counter frozen at zero while the
        // desktop moves under it reads as broken.
        state.touch_observers(None);
        let one = state.sequence(first);
        assert_eq!(state.sequence(second), one);
        assert!(one.raw() > proto::Sequence::ZERO.raw());

        // Delivery is what subscribing buys, and it stamps from the session's
        // own counter.
        state.publish([(first, focus_event(1))]);
        assert_eq!(state.take_events(first)[0].sequence, one);
        assert!(state.take_events(second).is_empty());

        // And one session's traffic never moves another's counter.
        state.touch_observers(None);
        assert!(state.sequence(first).raw() > one.raw());
        assert_eq!(state.sequence(first), state.sequence(second));
    }

    #[test]
    fn a_sequence_never_reports_activity_a_session_cannot_see() {
        // The sharp edge of a shared counter: a scoped session could read
        // out-of-scope activity out of the jumps, which is precisely what
        // scoping promises cannot happen.
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let watcher = observing_session(&mut state, &clients);
        let blind = proto::SessionId::new(2);
        state.open(blind, Grant::scoped(proto::CapabilitySet::EMPTY));
        state.observe_client(ClientId::new(1), AgentVisibility::Visible, |session| {
            session == watcher
        });

        state.touch_observers(Some(ClientId::new(1)));
        assert!(state.sequence(watcher).raw() > proto::Sequence::ZERO.raw());
        assert_eq!(
            state.sequence(blind),
            proto::Sequence::ZERO,
            "a session learns nothing about a client it cannot perceive"
        );
    }

    #[test]
    fn kind_filters_apply_only_to_filterable_kinds() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.subscribe(id, &[proto::EventKind::FocusChanged]);

        assert_eq!(
            state.subscribers(proto::EventKind::FocusChanged, None),
            vec![id]
        );
        assert!(
            state
                .subscribers(proto::EventKind::TitleChanged, None)
                .is_empty(),
            "a filtered kind is not delivered"
        );
        assert_eq!(
            state.subscribers(proto::EventKind::ResyncRequired, None),
            vec![id],
            "an agent cannot filter away the news that its world model is invalid"
        );
        assert_eq!(
            state.subscribers(proto::EventKind::SessionControl, None),
            vec![id]
        );
    }

    #[test]
    fn scope_and_visibility_filter_events_as_they_filter_snapshots() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::scoped(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        for client in clients.management_order().collect::<Vec<_>>() {
            let matches = client == ClientId::new(3);
            state.observe_client(client, AgentVisibility::Visible, |_| matches);
        }
        state.observe_client(ClientId::new(2), AgentVisibility::Hidden, |_| true);
        state.subscribe(id, &[]);

        assert_eq!(
            state.subscribers(proto::EventKind::TitleChanged, Some(ClientId::new(3))),
            vec![id]
        );
        assert!(
            state
                .subscribers(proto::EventKind::TitleChanged, Some(ClientId::new(1)))
                .is_empty(),
            "out of scope is out of the stream"
        );
        assert!(
            state
                .subscribers(proto::EventKind::TitleChanged, Some(ClientId::new(2)))
                .is_empty(),
            "hidden clients produce no events"
        );
    }

    #[test]
    fn an_overflowing_queue_becomes_one_resync_and_nothing_else() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.subscribe(id, &[]);
        for _ in 0..(super::MAX_QUEUED_EVENTS + 25) {
            state.publish([(id, focus_event(1))]);
        }
        let delivered = state.take_events(id);
        assert_eq!(delivered.len(), 1);
        let proto::Event::ResyncRequired { dropped } = delivered[0].event else {
            panic!("expected a resync, got {:?}", delivered[0].event);
        };
        assert!(dropped >= super::MAX_QUEUED_EVENTS as u64, "{dropped}");
        assert!(
            state.take_events(id).is_empty(),
            "the backlog behind a gap is never delivered"
        );

        // The stream resumes cleanly once the agent has re-snapshotted.
        state.publish([(id, focus_event(2))]);
        let delivered = state.take_events(id);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].event, focus_event(2));
    }

    #[test]
    fn resubscribing_discards_the_previous_backlog() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.subscribe(id, &[]);
        state.publish([(id, focus_event(1))]);
        state.subscribe(id, &[]);
        assert!(
            !state.has_events(id),
            "a fresh subscription starts from its own snapshot"
        );
    }

    #[test]
    fn a_closed_session_queues_nothing() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.subscribe(id, &[]);
        state.close(id);
        state.publish([(id, focus_event(1))]);
        assert!(state.take_events(id).is_empty());
        assert!(!state.any_subscribed());
    }

    #[test]
    fn an_empty_precondition_block_accepts_anything() {
        let (clients, _) = desktop();
        let state = AgentState::new();
        state
            .check_expects(ClientId::new(1), &proto::Expects::default(), &clients)
            .expect("nothing was claimed");
    }

    #[test]
    fn every_precondition_is_checked_against_the_live_desktop() {
        let (mut clients, _) = desktop();
        clients.focus(ClientId::new(1));
        let mut state = AgentState::new();
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        let client = ClientId::new(1);

        let truthful = proto::Expects {
            generation: Some(proto::Generation::FIRST),
            content: Some(proto::Rect::new(10, 10, 100, 120)),
            workspace: Some(proto::WorkspaceId::new(0)),
            focused: Some(true),
        };
        state
            .check_expects(client, &truthful, &clients)
            .expect("everything still holds");

        for wrong in [
            proto::Expects {
                generation: Some(proto::Generation::new(9)),
                ..proto::Expects::default()
            },
            proto::Expects {
                content: Some(proto::Rect::new(0, 0, 1, 1)),
                ..proto::Expects::default()
            },
            proto::Expects {
                workspace: Some(proto::WorkspaceId::new(3)),
                ..proto::Expects::default()
            },
            proto::Expects {
                focused: Some(false),
                ..proto::Expects::default()
            },
        ] {
            let error = state
                .check_expects(client, &wrong, &clients)
                .expect_err("stale");
            assert_eq!(error.code, proto::ErrorCode::StaleState);
            assert_eq!(error.current_generation, Some(proto::Generation::FIRST));
        }
    }

    #[test]
    fn a_bumped_generation_invalidates_what_the_agent_saw() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        let client = ClientId::new(1);
        let expects = proto::Expects {
            generation: Some(state.generation(client)),
            ..proto::Expects::default()
        };
        state
            .check_expects(client, &expects, &clients)
            .expect("fresh");

        let current = state.touch(client);
        let error = state
            .check_expects(client, &expects, &clients)
            .expect_err("stale");
        assert_eq!(error.code, proto::ErrorCode::StaleState);
        assert_eq!(
            error.current_generation,
            Some(current),
            "the refusal says exactly what to re-observe"
        );
    }

    #[test]
    fn a_sticky_client_matches_no_particular_workspace() {
        let (mut clients, _) = desktop();
        clients.assign_workspace(ClientId::new(1), WorkspaceAssignment::All);
        let state = AgentState::new();
        let expects = proto::Expects {
            workspace: Some(proto::WorkspaceId::new(0)),
            ..proto::Expects::default()
        };
        assert_eq!(
            state
                .check_expects(ClientId::new(1), &expects, &clients)
                .expect_err("stale")
                .code,
            proto::ErrorCode::StaleState
        );
    }

    #[test]
    fn regranting_keeps_the_stream_a_session_established() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        state.subscribe(id, &[]);
        state.publish([(id, focus_event(1))]);
        assert!(state.set_grant(id, Grant::denied()));
        assert!(
            state.has_events(id),
            "a re-granted session keeps what it was already told"
        );
        assert!(state.any_subscribed());
        assert_eq!(
            state
                .authorize(id, &proto::Call::DesktopSnapshot {})
                .expect_err("denied")
                .code,
            proto::ErrorCode::Denied
        );
    }

    #[test]
    fn freezing_is_not_revoking() {
        let (clients, _) = desktop();
        let mut state = AgentState::new();
        let id = observing_session(&mut state, &clients);
        assert_eq!(state.freeze_all(), vec![id]);
        assert!(state.any_frozen());
        assert_eq!(
            state
                .authorize(id, &proto::Call::DesktopSnapshot {})
                .expect_err("frozen")
                .code,
            proto::ErrorCode::SessionFrozen
        );
        assert!(
            state.freeze_all().is_empty(),
            "freezing twice changes nothing"
        );

        // The grant survived the freeze, so resuming restores it exactly.
        assert_eq!(state.resume_all(), vec![id]);
        assert!(!state.any_frozen());
        state
            .authorize(id, &proto::Call::DesktopSnapshot {})
            .expect("the grant survived");

        assert_eq!(state.revoke_all(), vec![id]);
        assert_eq!(
            state
                .authorize(id, &proto::Call::DesktopSnapshot {})
                .expect_err("revoked")
                .code,
            proto::ErrorCode::SessionRevoked
        );
        assert!(
            state.resume_all().is_empty(),
            "resuming never undoes a revocation"
        );
    }

    #[test]
    fn capabilities_the_human_must_see_are_recognized() {
        let mut state = AgentState::new();
        let observer = proto::SessionId::new(1);
        state.open(
            observer,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        assert!(!state.any_holds_visible_capability());
        let actor = proto::SessionId::new(2);
        state.open(
            actor,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::InputPointer)),
        );
        assert!(state.any_holds_visible_capability());
    }

    #[test]
    fn human_input_suppresses_agent_input_for_exactly_the_window() {
        use std::time::Duration;
        let window = Duration::from_millis(750);
        assert!(!super::is_suppressed(None, window));
        assert!(super::is_suppressed(Some(Duration::from_millis(0)), window));
        assert!(super::is_suppressed(
            Some(Duration::from_millis(749)),
            window
        ));
        assert!(!super::is_suppressed(
            Some(Duration::from_millis(750)),
            window
        ));
        assert!(
            !super::is_suppressed(Some(Duration::ZERO), Duration::ZERO),
            "a zero window disables suppression rather than blocking forever"
        );
    }

    #[test]
    fn snapshots_report_the_sequence_they_correspond_to() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        state.touch_observers(None);
        let snapshot = state.snapshot(id, &clients, &outputs, &Details);
        assert_eq!(snapshot.sequence, state.sequence(id));
        assert_eq!(snapshot.workspaces.len(), 4);
        assert_eq!(snapshot.current_workspace, proto::WorkspaceId::new(0));
        assert_eq!(snapshot.outputs.len(), 1);
        assert_eq!(
            snapshot.outputs[0].work_area,
            proto::Rect::new(0, 20, 800, 580)
        );
    }
}
