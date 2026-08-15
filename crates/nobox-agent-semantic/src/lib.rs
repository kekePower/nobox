//! Display-server-neutral disposable accessibility-helper lifecycle.

#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, DirBuilder};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nobox_agent_wire::{
    MAX_SEMANTIC_DEPTH, MAX_SEMANTIC_NAME_LEN, MAX_SEMANTIC_NODES, MAX_SEMANTIC_SCAN_NODES,
};
use serde::{Deserialize, Serialize};

const HELPER_VERSION: u8 = 1;
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const HARD_DEADLINE: Duration = Duration::from_millis(1_100);
const MAX_CONTINUATIONS: usize = 64;

/// One X11 screen-coordinate rectangle passed to the helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
}

/// Server-verified evidence for one authorized client.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Request {
    v: u8,
    pids: Vec<u32>,
    rects: Vec<Rect>,
    single_client: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    projection: Option<Projection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<Search>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Projection {
    root: u64,
    offset: u16,
    max_nodes: u16,
    max_depth: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Search {
    offset: u16,
    max_results: u16,
    query: nobox_agent_wire::SemanticQuery,
}

impl Search {
    pub const fn new(
        offset: u16,
        max_results: u16,
        query: nobox_agent_wire::SemanticQuery,
    ) -> Self {
        Self {
            offset,
            max_results,
            query,
        }
    }
}

impl Projection {
    pub const fn new(root: u64, offset: u16, max_nodes: u16, max_depth: u8) -> Self {
        Self {
            root,
            offset,
            max_nodes,
            max_depth,
        }
    }
}

impl Request {
    pub fn new(pid: u32, rects: Vec<Rect>, single_client: bool) -> Option<Self> {
        if pid == 0 || rects.is_empty() || rects.len() > 2 {
            return None;
        }
        Some(Self {
            v: HELPER_VERSION,
            pids: vec![pid],
            rects,
            single_client,
            projection: None,
            search: None,
        })
    }

    pub fn with_projection(mut self, projection: Projection) -> Self {
        self.projection = Some(projection);
        self
    }

    pub fn with_search(mut self, search: Search) -> Self {
        self.search = Some(search);
        self
    }
}

/// The only distinction the manager retains from helper output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Result {
    Matched(Match),
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Match {
    pub root: Root,
    pub nodes: Vec<Node>,
    pub next_offset: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completed {
    pub generation: u32,
    pub result: Result,
}

enum RunnerCommand {
    Start { generation: u32, request: Request },
    Cancel(u32),
    Stop,
}

/// One worker thread owns at most one disposable helper process globally.
pub struct Runner {
    commands: Sender<RunnerCommand>,
    completed: Receiver<Completed>,
    thread: Option<JoinHandle<()>>,
}

impl Runner {
    /// Starts the worker and invokes `wake` whenever a result becomes ready.
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        let helper = helper_path();
        let (commands, receiver) = mpsc::channel();
        let (completed_sender, completed) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("nobox-semantic-runner".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || run(receiver, completed_sender, wake, helper))?;
        Ok(Self {
            commands,
            completed,
            thread: Some(thread),
        })
    }

    pub fn start(&self, generation: u32, request: Request) -> bool {
        self.commands
            .send(RunnerCommand::Start {
                generation,
                request,
            })
            .is_ok()
    }

    pub fn cancel(&self, generation: u32) {
        let _ = self.commands.send(RunnerCommand::Cancel(generation));
    }

    pub fn take_completed(&self) -> Vec<Completed> {
        self.completed.try_iter().collect()
    }

    fn stop(&mut self) {
        let _ = self.commands.send(RunnerCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One validated helper request plus the private protocol state needed to
/// translate its result back onto the Agent Seat wire.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub projection: Option<Projection>,
    pub search: Option<Search>,
    kind: PreparedKind,
}

#[derive(Clone, Debug)]
enum PreparedKind {
    Root,
    Tree {
        tree_generation: nobox_agent_wire::TreeGeneration,
        root: u64,
        offset: u16,
        max_nodes: u16,
        max_depth: u8,
        source_continuation: Option<nobox_agent_wire::SemanticContinuation>,
    },
    Search {
        tree_generation: nobox_agent_wire::TreeGeneration,
        offset: u16,
        max_results: u16,
        query: nobox_agent_wire::SemanticQuery,
        source_continuation: Option<nobox_agent_wire::SemanticContinuation>,
    },
}

#[derive(Clone)]
enum Cursor {
    Tree {
        root: u64,
        offset: u16,
        max_depth: u8,
    },
    Search {
        offset: u16,
        query: nobox_agent_wire::SemanticQuery,
    },
}

struct Tree {
    generation: nobox_agent_wire::TreeGeneration,
    root: u64,
    public_by_internal: BTreeMap<u64, nobox_agent_wire::SemanticNodeId>,
    internal_by_public: BTreeMap<nobox_agent_wire::SemanticNodeId, u64>,
    next_node: u64,
    continuations: BTreeMap<nobox_agent_wire::SemanticContinuation, Cursor>,
    next_continuation: u64,
}

impl Tree {
    fn new(generation: nobox_agent_wire::TreeGeneration, root: u64) -> Self {
        let public = nobox_agent_wire::SemanticNodeId::new(1);
        Self {
            generation,
            root,
            public_by_internal: BTreeMap::from([(root, public)]),
            internal_by_public: BTreeMap::from([(public, root)]),
            next_node: 2,
            continuations: BTreeMap::new(),
            next_continuation: 1,
        }
    }

    fn public_id(&mut self, internal: u64) -> nobox_agent_wire::SemanticNodeId {
        if let Some(id) = self.public_by_internal.get(&internal) {
            return *id;
        }
        let id = nobox_agent_wire::SemanticNodeId::new(self.next_node);
        self.next_node = self.next_node.saturating_add(1);
        self.public_by_internal.insert(internal, id);
        self.internal_by_public.insert(id, internal);
        id
    }

    fn issue_continuation(&mut self, cursor: Cursor) -> nobox_agent_wire::SemanticContinuation {
        if self.next_continuation == u64::MAX {
            self.continuations.clear();
            self.next_continuation = 1;
        }
        if self.continuations.len() >= MAX_CONTINUATIONS
            && let Some(oldest) = self.continuations.keys().next().copied()
        {
            self.continuations.remove(&oldest);
        }
        let continuation = nobox_agent_wire::SemanticContinuation::new(self.next_continuation);
        self.next_continuation = self.next_continuation.saturating_add(1);
        self.continuations.insert(continuation, cursor);
        continuation
    }
}

/// Session-local opaque semantic-tree identities and continuations.
#[derive(Default)]
pub struct State {
    trees: BTreeMap<(nobox_agent_wire::SessionId, nobox_agent_wire::ClientId), Tree>,
}

impl State {
    /// Validates a semantic call against prior tree state before a helper runs.
    pub fn prepare(
        &self,
        session: nobox_agent_wire::SessionId,
        client: nobox_agent_wire::ClientId,
        call: &nobox_agent_wire::Call,
    ) -> std::result::Result<Prepared, nobox_agent_wire::ProtocolError> {
        let key = (session, client);
        match call {
            nobox_agent_wire::Call::ClientSemanticRoot { .. } => Ok(Prepared {
                projection: None,
                search: None,
                kind: PreparedKind::Root,
            }),
            nobox_agent_wire::Call::ClientSemanticTree {
                root,
                continuation,
                max_nodes,
                max_depth,
                ..
            } => {
                let Some(tree) = self.trees.get(&key) else {
                    return Err(nobox_agent_wire::ProtocolError::semantic_unavailable());
                };
                let (internal, offset, depth, source) = if let Some(continuation) = continuation {
                    let Some(Cursor::Tree {
                        root,
                        offset,
                        max_depth,
                    }) = tree.continuations.get(continuation).cloned()
                    else {
                        return Err(nobox_agent_wire::ProtocolError::stale_tree(tree.generation));
                    };
                    (root, offset, max_depth, Some(*continuation))
                } else if let Some(root) = root {
                    if root.tree != tree.generation {
                        return Err(nobox_agent_wire::ProtocolError::stale_tree(tree.generation));
                    }
                    let Some(internal) = tree.internal_by_public.get(&root.node).copied() else {
                        return Err(nobox_agent_wire::ProtocolError::stale_tree(tree.generation));
                    };
                    (internal, 0, *max_depth, None)
                } else {
                    (tree.root, 0, *max_depth, None)
                };
                Ok(Prepared {
                    projection: Some(Projection::new(internal, offset, *max_nodes, depth)),
                    search: None,
                    kind: PreparedKind::Tree {
                        tree_generation: tree.generation,
                        root: internal,
                        offset,
                        max_nodes: *max_nodes,
                        max_depth: depth,
                        source_continuation: source,
                    },
                })
            }
            nobox_agent_wire::Call::ClientSemanticFind {
                query,
                continuation,
                max_results,
                ..
            } => {
                let Some(tree) = self.trees.get(&key) else {
                    return Err(nobox_agent_wire::ProtocolError::semantic_unavailable());
                };
                let (offset, query, source) = if let Some(continuation) = continuation {
                    let Some(Cursor::Search { offset, query }) =
                        tree.continuations.get(continuation).cloned()
                    else {
                        return Err(nobox_agent_wire::ProtocolError::stale_tree(tree.generation));
                    };
                    (offset, query, Some(*continuation))
                } else {
                    (0, query.clone(), None)
                };
                Ok(Prepared {
                    projection: None,
                    search: Some(Search::new(offset, *max_results, query.clone())),
                    kind: PreparedKind::Search {
                        tree_generation: tree.generation,
                        offset,
                        max_results: *max_results,
                        query,
                        source_continuation: source,
                    },
                })
            }
            _ => Err(nobox_agent_wire::ProtocolError::new(
                nobox_agent_wire::ErrorCode::InvalidArgument,
                "the request is not a semantic call",
            )),
        }
    }

    /// Revalidates and translates one strictly matched helper result.
    pub fn complete(
        &mut self,
        session: nobox_agent_wire::SessionId,
        client: nobox_agent_wire::ClientId,
        client_generation: nobox_agent_wire::Generation,
        prepared: Prepared,
        matched: Match,
    ) -> nobox_agent_wire::Outcome {
        let key = (session, client);
        match prepared.kind {
            PreparedKind::Root => {
                if !matched.nodes.is_empty() || matched.next_offset.is_some() {
                    return semantic_unavailable();
                }
                let generation = self
                    .trees
                    .get(&key)
                    .map_or(nobox_agent_wire::TreeGeneration::FIRST, |tree| {
                        tree.generation.next()
                    });
                let tree = Tree::new(generation, matched.root.id);
                let handle = nobox_agent_wire::SemanticNodeHandle {
                    tree: generation,
                    node: nobox_agent_wire::SemanticNodeId::new(1),
                };
                self.trees.insert(key, tree);
                nobox_agent_wire::Outcome::Ok {
                    reply: nobox_agent_wire::Reply::SemanticTree {
                        page: nobox_agent_wire::SemanticTreePage {
                            client,
                            generation: client_generation,
                            tree_generation: generation,
                            root: handle,
                            nodes: vec![wire_node(&matched.root, handle)],
                            continuation: None,
                        },
                    },
                }
            }
            PreparedKind::Tree {
                tree_generation,
                root,
                offset,
                max_nodes,
                max_depth,
                source_continuation,
            } => {
                let Some(tree) = self.trees.get_mut(&key) else {
                    return semantic_unavailable();
                };
                if tree.generation != tree_generation {
                    return stale_tree(tree.generation);
                }
                if tree.root != matched.root.id {
                    let generation = tree.generation.next();
                    *tree = Tree::new(generation, matched.root.id);
                    return stale_tree(generation);
                }
                if !valid_projection(root, offset, max_nodes, max_depth, &matched) {
                    return semantic_unavailable();
                }
                let Some(root_node) = tree.public_by_internal.get(&root).copied() else {
                    return semantic_unavailable();
                };
                let mut nodes = Vec::with_capacity(matched.nodes.len());
                for node in matched.nodes {
                    let parent = node.parent.and_then(|parent| {
                        tree.public_by_internal.get(&parent).copied().map(|node| {
                            nobox_agent_wire::SemanticNodeHandle {
                                tree: tree.generation,
                                node,
                            }
                        })
                    });
                    if node.parent.is_some() && parent.is_none() {
                        return semantic_unavailable();
                    }
                    let handle = nobox_agent_wire::SemanticNodeHandle {
                        tree: tree.generation,
                        node: tree.public_id(node.id),
                    };
                    nodes.push(wire_projected_node(node, handle, parent));
                }
                if let Some(source) = source_continuation {
                    tree.continuations.remove(&source);
                }
                let continuation = matched.next_offset.map(|offset| {
                    tree.issue_continuation(Cursor::Tree {
                        root,
                        offset,
                        max_depth,
                    })
                });
                let root = nobox_agent_wire::SemanticNodeHandle {
                    tree: tree.generation,
                    node: root_node,
                };
                nobox_agent_wire::Outcome::Ok {
                    reply: nobox_agent_wire::Reply::SemanticTree {
                        page: nobox_agent_wire::SemanticTreePage {
                            client,
                            generation: client_generation,
                            tree_generation: tree.generation,
                            root,
                            nodes,
                            continuation,
                        },
                    },
                }
            }
            PreparedKind::Search {
                tree_generation,
                offset,
                max_results,
                query,
                source_continuation,
            } => {
                let Some(tree) = self.trees.get_mut(&key) else {
                    return semantic_unavailable();
                };
                if tree.generation != tree_generation {
                    return stale_tree(tree.generation);
                }
                if tree.root != matched.root.id {
                    let generation = tree.generation.next();
                    *tree = Tree::new(generation, matched.root.id);
                    return stale_tree(generation);
                }
                if !valid_search(offset, max_results, &query, &matched) {
                    return semantic_unavailable();
                }
                let nodes = matched
                    .nodes
                    .into_iter()
                    .map(|node| {
                        let parent = node.parent.and_then(|parent| {
                            tree.public_by_internal.get(&parent).copied().map(|node| {
                                nobox_agent_wire::SemanticNodeHandle {
                                    tree: tree.generation,
                                    node,
                                }
                            })
                        });
                        let handle = nobox_agent_wire::SemanticNodeHandle {
                            tree: tree.generation,
                            node: tree.public_id(node.id),
                        };
                        wire_projected_node(node, handle, parent)
                    })
                    .collect();
                if let Some(source) = source_continuation {
                    tree.continuations.remove(&source);
                }
                let continuation = matched
                    .next_offset
                    .map(|offset| tree.issue_continuation(Cursor::Search { offset, query }));
                nobox_agent_wire::Outcome::Ok {
                    reply: nobox_agent_wire::Reply::SemanticMatches {
                        page: nobox_agent_wire::SemanticSearchPage {
                            client,
                            generation: client_generation,
                            tree_generation: tree.generation,
                            matches: nodes,
                            continuation,
                        },
                    },
                }
            }
        }
    }

    pub fn forget_client(&mut self, client: nobox_agent_wire::ClientId) {
        self.trees.retain(|(_, candidate), _| *candidate != client);
    }

    pub fn forget_session(&mut self, session: nobox_agent_wire::SessionId) {
        self.trees.retain(|(candidate, _), _| *candidate != session);
    }
}

fn wire_node(
    root: &Root,
    handle: nobox_agent_wire::SemanticNodeHandle,
) -> nobox_agent_wire::SemanticNode {
    nobox_agent_wire::SemanticNode {
        handle,
        parent: None,
        depth: 0,
        role: root.role,
        name: root.name.clone(),
        description: None,
        value: None,
        states: root.states.clone(),
        bounds: Some(root.bounds),
        child_count: root.child_count,
        relations: Vec::new(),
    }
}

fn wire_projected_node(
    node: Node,
    handle: nobox_agent_wire::SemanticNodeHandle,
    parent: Option<nobox_agent_wire::SemanticNodeHandle>,
) -> nobox_agent_wire::SemanticNode {
    nobox_agent_wire::SemanticNode {
        handle,
        parent,
        depth: node.depth,
        role: node.role,
        name: node.name,
        description: None,
        value: None,
        states: node.states,
        bounds: node.bounds,
        child_count: node.child_count,
        relations: Vec::new(),
    }
}

fn valid_projection(
    root: u64,
    offset: u16,
    max_nodes: u16,
    max_depth: u8,
    matched: &Match,
) -> bool {
    if matched.nodes.is_empty() || matched.nodes.len() > usize::from(max_nodes) {
        return false;
    }
    let Some(returned) = u16::try_from(matched.nodes.len()).ok() else {
        return false;
    };
    let Some(expected_next) = offset.checked_add(returned) else {
        return false;
    };
    if expected_next > nobox_agent_wire::MAX_SEMANTIC_SCAN_NODES
        || matched
            .next_offset
            .is_some_and(|next| next != expected_next || returned != max_nodes)
        || (offset == 0
            && !matches!(matched.nodes.first(), Some(node) if node.id == root && node.parent.is_none() && node.depth == 0))
    {
        return false;
    }
    let mut depths = BTreeMap::<u64, u8>::new();
    matched.nodes.iter().enumerate().all(|(index, node)| {
        if node.depth > max_depth
            || ((offset > 0 || index > 0) && (node.parent.is_none() || node.depth == 0))
            || node
                .parent
                .and_then(|parent| depths.get(&parent))
                .is_some_and(|depth| depth.checked_add(1) != Some(node.depth))
        {
            return false;
        }
        depths.insert(node.id, node.depth);
        true
    })
}

fn valid_search(
    offset: u16,
    max_results: u16,
    query: &nobox_agent_wire::SemanticQuery,
    matched: &Match,
) -> bool {
    matched.nodes.len() <= usize::from(max_results)
        && !matched.next_offset.is_some_and(|next| {
            next <= offset
                || next > nobox_agent_wire::MAX_SEMANTIC_SCAN_NODES
                || matched.nodes.len() != usize::from(max_results)
        })
        && matched.nodes.iter().all(|node| {
            query.name.as_ref().is_none_or(|needle| {
                node.name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase().contains(&needle.to_lowercase()))
            }) && (query.roles.is_empty() || query.roles.contains(&node.role))
                && query.states.iter().all(|state| node.states.contains(state))
        })
}

fn semantic_unavailable() -> nobox_agent_wire::Outcome {
    nobox_agent_wire::Outcome::Error {
        error: nobox_agent_wire::ProtocolError::semantic_unavailable(),
    }
}

fn stale_tree(generation: nobox_agent_wire::TreeGeneration) -> nobox_agent_wire::Outcome {
    nobox_agent_wire::Outcome::Error {
        error: nobox_agent_wire::ProtocolError::stale_tree(generation),
    }
}

fn run(
    commands: Receiver<RunnerCommand>,
    completed: Sender<Completed>,
    wake: Arc<dyn Fn() + Send + Sync>,
    helper: Option<PathBuf>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            RunnerCommand::Start {
                generation,
                request,
            } => {
                let Some(helper) = helper.as_deref() else {
                    complete(&completed, &wake, generation, Result::Unavailable);
                    continue;
                };
                if !run_one(&commands, &completed, &wake, helper, generation, &request) {
                    break;
                }
            }
            RunnerCommand::Cancel(_) => {}
            RunnerCommand::Stop => break,
        }
    }
}

fn run_one(
    commands: &Receiver<RunnerCommand>,
    completed: &Sender<Completed>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    helper: &Path,
    generation: u32,
    request: &Request,
) -> bool {
    let Some(private_dir) = private_directory(generation) else {
        complete(completed, wake, generation, Result::Unavailable);
        return true;
    };
    let outcome = spawn_helper(helper, &private_dir, request);
    let (mut child, output_thread) = match outcome {
        Ok(value) => value,
        Err(()) => {
            let _ = fs::remove_dir(&private_dir);
            complete(completed, wake, generation, Result::Unavailable);
            return true;
        }
    };
    let deadline = Instant::now() + HARD_DEADLINE;
    let mut keep_running = true;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if Instant::now() >= deadline {
            break None;
        }
        match commands.recv_timeout(POLL_INTERVAL) {
            Ok(RunnerCommand::Cancel(value)) if value == generation => {
                cancelled = true;
                break None;
            }
            Ok(RunnerCommand::Start {
                generation,
                request: _,
            }) => complete(completed, wake, generation, Result::Unavailable),
            Ok(RunnerCommand::Stop) => {
                cancelled = true;
                keep_running = false;
                break None;
            }
            Ok(RunnerCommand::Cancel(_)) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                cancelled = true;
                keep_running = false;
                break None;
            }
        }
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let output = output_thread.join().unwrap_or_default();
    let _ = fs::remove_dir(&private_dir);
    if !cancelled {
        let result = status
            .filter(std::process::ExitStatus::success)
            .map_or(Result::Unavailable, |_| parse_output(&output));
        complete(completed, wake, generation, result);
    }
    keep_running
}

fn spawn_helper(
    helper: &Path,
    private_dir: &Path,
    request: &Request,
) -> std::result::Result<(Child, JoinHandle<Vec<u8>>), ()> {
    let mut command = Command::new(helper);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .current_dir(private_dir)
        .env_clear();
    for key in [
        "AT_SPI_BUS_ADDRESS",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().map_err(|_| ())?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(());
    };
    let output_thread = match thread::Builder::new()
        .name("nobox-semantic-output".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut output = Vec::new();
            let _ = stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut output);
            output
        }) {
        Ok(thread) => thread,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(());
        }
    };
    let encoded = match serde_json::to_vec(request) {
        Ok(encoded) => encoded,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = output_thread.join();
            return Err(());
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = output_thread.join();
        return Err(());
    };
    if stdin.write_all(&encoded).is_err() || stdin.write_all(b"\n").is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = output_thread.join();
        return Err(());
    }
    drop(stdin);
    Ok((child, output_thread))
}

fn complete(
    completed: &Sender<Completed>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    generation: u32,
    result: Result,
) {
    if completed.send(Completed { generation, result }).is_ok() {
        wake();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    v: u8,
    status: WireStatus,
    #[serde(default)]
    root: Option<Root>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    next_offset: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub id: u64,
    pub role: nobox_agent_wire::SemanticRole,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub states: Vec<nobox_agent_wire::SemanticState>,
    pub bounds: nobox_agent_wire::Rect,
    pub child_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: u64,
    #[serde(default)]
    pub parent: Option<u64>,
    pub depth: u8,
    pub role: nobox_agent_wire::SemanticRole,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub states: Vec<nobox_agent_wire::SemanticState>,
    #[serde(default)]
    pub bounds: Option<nobox_agent_wire::Rect>,
    pub child_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireStatus {
    Matched,
    Ambiguous,
    Unavailable,
    Invalid,
}

fn parse_output(output: &[u8]) -> Result {
    if output.len() > usize::try_from(MAX_OUTPUT_BYTES).unwrap_or(usize::MAX) {
        return Result::Unavailable;
    }
    match serde_json::from_slice::<WireResponse>(output) {
        Ok(WireResponse {
            v: HELPER_VERSION,
            status: WireStatus::Matched,
            root: Some(root),
            nodes,
            next_offset,
        }) if valid_root(&root)
            && valid_nodes(&nodes)
            && next_offset.is_none_or(|offset| (1..=MAX_SEMANTIC_SCAN_NODES).contains(&offset)) =>
        {
            Result::Matched(Match {
                root,
                nodes,
                next_offset,
            })
        }
        Ok(WireResponse {
            status: WireStatus::Ambiguous | WireStatus::Unavailable | WireStatus::Invalid,
            ..
        })
        | Ok(_)
        | Err(_) => Result::Unavailable,
    }
}

fn valid_root(root: &Root) -> bool {
    root.id != 0
        && root
            .name
            .as_ref()
            .is_none_or(|name| name.len() <= MAX_SEMANTIC_NAME_LEN)
        && ordered_states(&root.states)
        && root.bounds.width > 0
        && root.bounds.height > 0
}

fn valid_nodes(nodes: &[Node]) -> bool {
    if nodes.len() > usize::from(MAX_SEMANTIC_NODES) {
        return false;
    }
    let mut ids = std::collections::BTreeSet::new();
    nodes.iter().all(|node| {
        node.id != 0
            && node.parent != Some(node.id)
            && node.depth <= MAX_SEMANTIC_DEPTH
            && node
                .name
                .as_ref()
                .is_none_or(|name| name.len() <= MAX_SEMANTIC_NAME_LEN)
            && ordered_states(&node.states)
            && node
                .bounds
                .is_none_or(|bounds| bounds.width > 0 && bounds.height > 0)
            && ids.insert(node.id)
    })
}

fn ordered_states(states: &[nobox_agent_wire::SemanticState]) -> bool {
    states.windows(2).all(|states| states[0] < states[1])
}

fn helper_path() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    let directory = executable.parent()?;
    let sibling = directory.join("agent-semantic-helper");
    if sibling.is_file() {
        return Some(sibling);
    }
    let installed = directory
        .parent()?
        .join("libexec/nobox/agent-semantic-helper");
    installed.is_file().then_some(installed)
}

fn private_directory(generation: u32) -> Option<PathBuf> {
    let parent = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)?
        .join("nobox");
    for attempt in 0..8_u8 {
        let path = parent.join(format!(
            "semantic-{}-{generation}-{attempt}",
            std::process::id()
        ));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => return Some(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        Match, Node, Projection, Rect, Request, Result, Root, Search, State, parse_output,
    };

    #[test]
    fn helper_request_is_compact_and_deterministic() {
        let request = Request::new(
            1234,
            vec![Rect {
                x: 20,
                y: 40,
                width: 900,
                height: 600,
            }],
            true,
        )
        .expect("valid request");
        assert_eq!(
            serde_json::to_string(&request).expect("serialize request"),
            r#"{"v":1,"pids":[1234],"rects":[{"x":20,"y":40,"width":900,"height":600}],"single_client":true}"#
        );
    }

    #[test]
    fn only_one_strict_matched_response_survives_translation() {
        assert_eq!(
            parse_output(b"{\"v\":1,\"status\":\"matched\",\"root\":{\"id\":7,\"role\":\"window\",\"name\":\"Demo\",\"states\":[\"visible\"],\"bounds\":{\"x\":0,\"y\":0,\"width\":900,\"height\":600},\"child_count\":2}}\n"),
            Result::Matched(super::Match {
                root: super::Root {
                    id: 7,
                    role: nobox_agent_wire::SemanticRole::Window,
                    name: Some("Demo".to_owned()),
                    states: vec![nobox_agent_wire::SemanticState::Visible],
                    bounds: nobox_agent_wire::Rect::new(0, 0, 900, 600),
                    child_count: 2,
                },
                nodes: Vec::new(),
                next_offset: None,
            })
        );
        for unavailable in [
            br#"{"v":1,"status":"ambiguous"}"#.as_slice(),
            br#"{"v":2,"status":"matched"}"#,
            br#"{"v":1,"status":"matched","name":"secret"}"#,
            br#"{"v":1,"status":"missing"}"#,
        ] {
            assert_eq!(parse_output(unavailable), Result::Unavailable);
        }
        assert_eq!(
            parse_output(&vec![b' '; 1024 * 1024 + 1]),
            Result::Unavailable
        );
    }

    #[test]
    fn projection_request_and_page_are_bounded() {
        let request = Request::new(
            1234,
            vec![Rect {
                x: 20,
                y: 40,
                width: 900,
                height: 600,
            }],
            true,
        )
        .expect("valid request")
        .with_projection(Projection::new(7, 2, 1, 3));
        assert_eq!(
            serde_json::to_string(&request).expect("serialize request"),
            r#"{"v":1,"pids":[1234],"rects":[{"x":20,"y":40,"width":900,"height":600}],"single_client":true,"projection":{"root":7,"offset":2,"max_nodes":1,"max_depth":3}}"#
        );

        assert!(matches!(
            parse_output(br#"{"v":1,"status":"matched","root":{"id":7,"role":"window","states":["visible"],"bounds":{"x":0,"y":0,"width":900,"height":600},"child_count":1},"nodes":[{"id":7,"depth":0,"role":"window","states":["visible"],"bounds":{"x":0,"y":0,"width":900,"height":600},"child_count":1}],"next_offset":1}"#),
            Result::Matched(_)
        ));
        for unavailable in [
            br#"{"v":1,"status":"matched","root":{"id":7,"role":"window","states":["visible","visible"],"bounds":{"x":0,"y":0,"width":900,"height":600},"child_count":1}}"#.as_slice(),
            br#"{"v":1,"status":"matched","root":{"id":7,"role":"window","bounds":{"x":0,"y":0,"width":0,"height":600},"child_count":1}}"#,
            br#"{"v":1,"status":"matched","root":{"id":7,"role":"window","bounds":{"x":0,"y":0,"width":900,"height":600},"child_count":1},"nodes":[{"id":8,"parent":8,"depth":1,"role":"button","child_count":0}]}"#,
            br#"{"v":1,"status":"matched","root":{"id":7,"role":"window","bounds":{"x":0,"y":0,"width":900,"height":600},"child_count":1},"next_offset":4097}"#,
        ] {
            assert_eq!(parse_output(unavailable), Result::Unavailable);
        }
    }

    #[test]
    fn search_request_is_compact_and_typed() {
        let request = Request::new(
            1234,
            vec![Rect {
                x: 20,
                y: 40,
                width: 900,
                height: 600,
            }],
            true,
        )
        .expect("valid request")
        .with_search(Search::new(
            3,
            8,
            nobox_agent_wire::SemanticQuery {
                name: Some("play".to_owned()),
                roles: vec![nobox_agent_wire::SemanticRole::Button],
                states: vec![nobox_agent_wire::SemanticState::Visible],
            },
        ));
        assert_eq!(
            serde_json::to_string(&request).expect("serialize request"),
            r#"{"v":1,"pids":[1234],"rects":[{"x":20,"y":40,"width":900,"height":600}],"single_client":true,"search":{"offset":3,"max_results":8,"query":{"name":"play","roles":["button"],"states":["visible"]}}}"#
        );
    }

    #[test]
    fn semantic_state_keeps_backend_ids_private_and_generation_scoped() {
        let session = nobox_agent_wire::SessionId::new(7);
        let client = nobox_agent_wire::ClientId::new(11);
        let mut state = State::default();
        let root_call = nobox_agent_wire::Call::ClientSemanticRoot { client };
        let prepared = state.prepare(session, client, &root_call).unwrap();
        let root = Root {
            id: 91,
            role: nobox_agent_wire::SemanticRole::Window,
            name: Some("Demo".to_owned()),
            states: vec![nobox_agent_wire::SemanticState::Visible],
            bounds: nobox_agent_wire::Rect::new(0, 0, 320, 200),
            child_count: 1,
        };
        let outcome = state.complete(
            session,
            client,
            nobox_agent_wire::Generation::FIRST,
            prepared,
            Match {
                root: root.clone(),
                nodes: Vec::new(),
                next_offset: None,
            },
        );
        let nobox_agent_wire::Outcome::Ok {
            reply: nobox_agent_wire::Reply::SemanticTree { page },
        } = outcome
        else {
            panic!("root projection failed");
        };
        assert_eq!(page.root.node.raw(), 1);
        assert_ne!(page.root.node.raw(), root.id);

        let call = nobox_agent_wire::Call::ClientSemanticTree {
            client,
            root: Some(page.root),
            continuation: None,
            max_nodes: 1,
            max_depth: 1,
        };
        let prepared = state.prepare(session, client, &call).unwrap();
        let outcome = state.complete(
            session,
            client,
            nobox_agent_wire::Generation::FIRST,
            prepared,
            Match {
                root,
                nodes: vec![Node {
                    id: 91,
                    parent: None,
                    depth: 0,
                    role: nobox_agent_wire::SemanticRole::Window,
                    name: Some("Demo".to_owned()),
                    states: vec![nobox_agent_wire::SemanticState::Visible],
                    bounds: Some(nobox_agent_wire::Rect::new(0, 0, 320, 200)),
                    child_count: 1,
                }],
                next_offset: Some(1),
            },
        );
        assert!(matches!(
            outcome,
            nobox_agent_wire::Outcome::Ok {
                reply: nobox_agent_wire::Reply::SemanticTree { .. }
            }
        ));
    }
}
