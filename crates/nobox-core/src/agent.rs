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

use std::collections::{BTreeMap, BTreeSet};

use agent_seat_proto as proto;

use crate::{
    Client, ClientId, ClientSet, Geometry, OutputId, OutputSet, WorkspaceAssignment, WorkspaceId,
};

/// How visible a client is to agent sessions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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

/// One agent session's policy state.
#[derive(Clone, Debug)]
pub struct AgentSession {
    grant: Grant,
    status: SessionStatus,
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
}

/// Every agent session, and the counters their world models depend on.
#[derive(Clone, Debug, Default)]
pub struct AgentState {
    sessions: BTreeMap<proto::SessionId, AgentSession>,
    visibility: BTreeMap<ClientId, AgentVisibility>,
    generations: BTreeMap<ClientId, proto::Generation>,
    sequence: proto::Sequence,
}

impl AgentState {
    /// Returns an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the manager's current sequence number.
    #[must_use]
    pub const fn sequence(&self) -> proto::Sequence {
        self.sequence
    }

    /// Advances and returns the sequence. Every snapshot and every event is
    /// stamped from this one counter, so an agent can order them absolutely.
    pub fn advance(&mut self) -> proto::Sequence {
        self.sequence = self.sequence.next();
        self.sequence
    }

    /// Opens a session with an already-decided grant.
    pub fn open(&mut self, session: proto::SessionId, grant: Grant) {
        self.sessions.insert(
            session,
            AgentSession {
                grant,
                status: SessionStatus::Active,
            },
        );
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

    /// Sets a session's status.
    pub fn set_status(&mut self, session: proto::SessionId, status: SessionStatus) -> bool {
        self.sessions
            .get_mut(&session)
            .map(|state| state.status = status)
            .is_some()
    }

    /// Records a client's configured visibility and its scope membership in
    /// every session whose grant is scoped to it.
    pub fn observe_client(
        &mut self,
        client: ClientId,
        visibility: AgentVisibility,
        mut in_scope: impl FnMut(proto::SessionId) -> bool,
    ) {
        self.visibility.insert(client, visibility);
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

    /// Bumps a client's generation and advances the sequence, returning both.
    pub fn touch(&mut self, client: ClientId) -> (proto::Generation, proto::Sequence) {
        let generation = self
            .generations
            .entry(client)
            .and_modify(|generation| *generation = generation.next())
            .or_insert(proto::Generation::FIRST);
        let generation = *generation;
        (generation, self.advance())
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
            state: client_state(clients, managed),
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
            sequence: self.sequence,
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

fn client_state(clients: &ClientSet, client: &Client) -> proto::ClientState {
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

        let (generation, sequence) = state.touch(ClientId::new(1));
        assert_eq!(generation, proto::Generation::new(2));
        assert_eq!(sequence, proto::Sequence::new(1));
        assert_eq!(state.generation(ClientId::new(2)), proto::Generation::FIRST);

        let (generation, sequence) = state.touch(ClientId::new(1));
        assert_eq!(generation, proto::Generation::new(3));
        assert_eq!(sequence, proto::Sequence::new(2));
        assert_eq!(state.sequence(), proto::Sequence::new(2));
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

    #[test]
    fn snapshots_report_the_sequence_they_correspond_to() {
        let (clients, outputs) = desktop();
        let mut state = AgentState::new();
        let id = session(
            &mut state,
            Grant::new(proto::CapabilitySet::EMPTY.with(proto::Capability::ObserveStructure)),
        );
        observe_all(&mut state, &clients, AgentVisibility::Visible);
        state.touch(ClientId::new(1));
        let snapshot = state.snapshot(id, &clients, &outputs, &Details);
        assert_eq!(snapshot.sequence, state.sequence());
        assert_eq!(snapshot.workspaces.len(), 4);
        assert_eq!(snapshot.current_workspace, proto::WorkspaceId::new(0));
        assert_eq!(snapshot.outputs.len(), 1);
        assert_eq!(
            snapshot.outputs[0].work_area,
            proto::Rect::new(0, 20, 800, 580)
        );
    }
}
