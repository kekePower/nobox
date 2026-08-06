//! One-shot bounded AT-SPI root correlation process.

use std::future::Future;
use std::io::{self, Read, Write};
use std::time::Duration;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    convert::TryInto,
};

use agent_semantic_helper::{
    Candidate, Correlation, DiscoveryRequest, DiscoveryResponse, DiscoveryStatus, HELPER_VERSION,
    MAX_APPLICATIONS, MAX_INPUT_BYTES, MAX_SCANNED_NODES, MAX_TOPLEVELS, ProjectedNode,
    ProjectedRole, ProjectedState, ProjectionRequest, RootProjection, SearchQuery, SearchRequest,
    TargetRect, TopLevelRole, correlate, correlate_candidate,
};
use atspi::proxy::accessible::ObjectRefExt;
use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{AccessibilityConnection, Role, State};
use futures_lite::future;
use rustix::process::{Resource, Rlimit, setrlimit};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch, apply_filter_all_threads,
};
use serde::Deserialize;
use zbus::names::BusName;

const TOTAL_DISCOVERY_MS: u64 = 1_000;
const CALL_MS: u64 = 150;
const MAX_NAME_BYTES: usize = 512;

#[cfg(target_arch = "x86_64")]
const TARGET_ARCH: TargetArch = TargetArch::x86_64;
#[cfg(target_arch = "aarch64")]
const TARGET_ARCH: TargetArch = TargetArch::aarch64;
#[cfg(target_arch = "riscv64")]
const TARGET_ARCH: TargetArch = TargetArch::riscv64;
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
compile_error!("agent-semantic-helper requires a seccompiler-supported Linux architecture");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureEnvelope {
    target: DiscoveryRequest,
    candidates: Vec<Candidate>,
    complete: bool,
}

fn apply_process_limits() -> Result<(), ()> {
    rustix::thread::set_no_new_privs(true).map_err(|_| ())?;
    for (resource, value) in [
        (Resource::Cpu, 2),
        (Resource::Fsize, 64 * 1024),
        (Resource::Nofile, 64),
        (Resource::As, 512 * 1024 * 1024),
        (Resource::Core, 0),
    ] {
        setrlimit(
            resource,
            Rlimit {
                current: Some(value),
                maximum: Some(value),
            },
        )
        .map_err(|_| ())?;
    }
    Ok(())
}

fn apply_syscall_sandbox() -> Result<(), ()> {
    // D-Bus sockets and the async reactor already exist. There is no socket,
    // connect, open, exec, clone, or process-control syscall in this list.
    let allowed = [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_close,
        libc::SYS_fcntl,
        libc::SYS_ioctl,
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_wait,
        libc::SYS_epoll_pwait,
        libc::SYS_eventfd2,
        libc::SYS_timerfd_settime,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_futex,
        libc::SYS_sched_yield,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_getrandom,
        libc::SYS_getsockopt,
        libc::SYS_getpeername,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_mremap,
        libc::SYS_munmap,
        libc::SYS_madvise,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_restart_syscall,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    let rules = allowed
        .into_iter()
        .map(|syscall| (syscall, Vec::<SeccompRule>::new()))
        .collect::<BTreeMap<_, _>>();
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
        TARGET_ARCH,
    )
    .map_err(|_| ())?;
    let program: BpfProgram = filter.try_into().map_err(|_| ())?;
    apply_filter_all_threads(&program).map_err(|_| ())
}

fn read_input() -> Result<Vec<u8>, ()> {
    let maximum = u64::try_from(MAX_INPUT_BYTES + 1).map_err(|_| ())?;
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .take(maximum)
        .read_to_end(&mut input)
        .map_err(|_| ())?;
    if input.is_empty() || input.len() > MAX_INPUT_BYTES {
        return Err(());
    }
    Ok(input)
}

async fn timeout<F, T>(duration: Duration, operation: F) -> Result<T, ()>
where
    F: Future<Output = T>,
{
    future::race(async { Ok(operation.await) }, async {
        async_io::Timer::after(duration).await;
        Err(())
    })
    .await
}

async fn bus_pid(
    proxy: &zbus::fdo::DBusProxy<'_>,
    reference: &atspi::ObjectRefOwned,
) -> Result<u32, ()> {
    let name = reference.name().ok_or(())?.clone();
    timeout(
        Duration::from_millis(CALL_MS),
        proxy.get_connection_unix_process_id(BusName::Unique(name)),
    )
    .await?
    .map_err(|_| ())
}

fn top_level_role(role: Role) -> Option<TopLevelRole> {
    match role {
        Role::Dialog => Some(TopLevelRole::Dialog),
        Role::Filler => Some(TopLevelRole::Filler),
        Role::Frame => Some(TopLevelRole::Frame),
        Role::Window => Some(TopLevelRole::Window),
        _ => None,
    }
}

struct Discovered {
    candidate: Candidate,
    reference: atspi::ObjectRefOwned,
    role: Role,
    states: atspi::StateSet,
}

fn bounded_text(mut value: String) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if value.len() > MAX_NAME_BYTES {
        let mut boundary = MAX_NAME_BYTES;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    (!value.is_empty()).then_some(value)
}

fn projected_role(role: Role) -> ProjectedRole {
    match role {
        Role::Application => ProjectedRole::Application,
        Role::Dialog => ProjectedRole::Dialog,
        Role::Filler | Role::Frame | Role::Window => ProjectedRole::Window,
        Role::DocumentEmail
        | Role::DocumentFrame
        | Role::DocumentPresentation
        | Role::DocumentSpreadsheet
        | Role::DocumentText
        | Role::DocumentWeb
        | Role::HTMLContainer => ProjectedRole::Document,
        Role::Heading | Role::Header => ProjectedRole::Heading,
        Role::Paragraph => ProjectedRole::Paragraph,
        Role::Link => ProjectedRole::Link,
        Role::Button | Role::PushButtonMenu | Role::ToggleButton => ProjectedRole::Button,
        Role::CheckBox => ProjectedRole::CheckBox,
        Role::RadioButton => ProjectedRole::RadioButton,
        Role::ComboBox => ProjectedRole::ComboBox,
        Role::AcceleratorLabel | Role::Caption | Role::Label | Role::Static | Role::Text => {
            ProjectedRole::Text
        }
        Role::Editbar | Role::Entry | Role::PasswordText => ProjectedRole::Entry,
        Role::DirectoryPane | Role::List | Role::ListBox | Role::Tree => ProjectedRole::List,
        Role::ListItem | Role::TreeItem => ProjectedRole::ListItem,
        Role::Table | Role::TreeTable => ProjectedRole::Table,
        Role::TableCell | Role::TableColumnHeader | Role::TableRow | Role::TableRowHeader => {
            ProjectedRole::Cell
        }
        Role::Icon | Role::Image | Role::ImageMap => ProjectedRole::Image,
        Role::Video => ProjectedRole::Video,
        Role::Audio => ProjectedRole::Audio,
        Role::Menu | Role::MenuBar | Role::PopupMenu => ProjectedRole::Menu,
        Role::CheckMenuItem | Role::MenuItem | Role::RadioMenuItem | Role::TearoffMenuItem => {
            ProjectedRole::MenuItem
        }
        Role::PageTab => ProjectedRole::Tab,
        Role::PageTabList => ProjectedRole::TabList,
        Role::ToolBar => ProjectedRole::Toolbar,
        Role::InfoBar | Role::Notification | Role::StatusBar => ProjectedRole::Status,
        Role::Dial | Role::Slider => ProjectedRole::Slider,
        Role::SpinButton => ProjectedRole::SpinButton,
        Role::LevelBar | Role::ProgressBar => ProjectedRole::Progress,
        Role::ScrollBar => ProjectedRole::ScrollBar,
        Role::Separator => ProjectedRole::Separator,
        Role::ToolTip => ProjectedRole::Tooltip,
        Role::Grouping | Role::Panel | Role::RootPane => ProjectedRole::Group,
        Role::Article | Role::Page | Role::Section => ProjectedRole::Section,
        Role::Form => ProjectedRole::Form,
        Role::Landmark => ProjectedRole::Landmark,
        _ => ProjectedRole::Unknown,
    }
}

fn projected_states(states: &atspi::StateSet, role: Role) -> Vec<ProjectedState> {
    let mut projected = Vec::new();
    if states.contains(State::Active) {
        projected.push(ProjectedState::Active);
    }
    if states.contains(State::Busy) {
        projected.push(ProjectedState::Busy);
    }
    if states.contains(State::Checked) {
        projected.push(ProjectedState::Checked);
    }
    if states.contains(State::Collapsed) {
        projected.push(ProjectedState::Collapsed);
    }
    if !states.contains(State::Enabled) {
        projected.push(ProjectedState::Disabled);
    }
    if states.contains(State::Editable) {
        projected.push(ProjectedState::Editable);
    }
    if states.contains(State::Expanded) {
        projected.push(ProjectedState::Expanded);
    }
    if states.contains(State::Focusable) {
        projected.push(ProjectedState::Focusable);
    }
    if states.contains(State::Focused) {
        projected.push(ProjectedState::Focused);
    }
    if states.contains(State::InvalidEntry) {
        projected.push(ProjectedState::Invalid);
    }
    if states.contains(State::Modal) {
        projected.push(ProjectedState::Modal);
    }
    if states.contains(State::MultiLine) {
        projected.push(ProjectedState::Multiline);
    }
    if !states.contains(State::Showing) {
        projected.push(ProjectedState::Offscreen);
    }
    if states.contains(State::Pressed) {
        projected.push(ProjectedState::Pressed);
    }
    if role == Role::PasswordText {
        projected.push(ProjectedState::Protected);
    }
    if states.contains(State::ReadOnly) {
        projected.push(ProjectedState::ReadOnly);
    }
    if states.contains(State::Required) {
        projected.push(ProjectedState::Required);
    }
    if states.contains(State::Selected) {
        projected.push(ProjectedState::Selected);
    }
    if states.contains(State::Selectable) {
        projected.push(ProjectedState::Selectable);
    }
    if states.contains(State::Visible) {
        projected.push(ProjectedState::Visible);
    }
    projected
}

fn object_id(reference: &atspi::ObjectRefOwned) -> Result<u64, ()> {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let name = reference.name_as_str().ok_or(())?;
    let mut hash = OFFSET;
    for byte in name
        .bytes()
        .chain(std::iter::once(0))
        .chain(reference.path().as_str().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (hash != 0).then_some(hash).ok_or(())
}

fn register_object(
    reference: &atspi::ObjectRefOwned,
    identities: &mut BTreeMap<u64, (String, String)>,
) -> Result<u64, ()> {
    let id = object_id(reference)?;
    let identity = (
        reference.name_as_str().ok_or(())?.to_owned(),
        reference.path().as_str().to_owned(),
    );
    match identities.get(&id) {
        Some(existing) if existing != &identity => Err(()),
        Some(_) => Ok(id),
        None => {
            identities.insert(id, identity);
            Ok(id)
        }
    }
}

#[derive(Default)]
struct ProjectionScan {
    identities: BTreeMap<u64, (String, String)>,
    pids: BTreeMap<String, u32>,
    count: u16,
}

impl ProjectionScan {
    fn inspect(&mut self) -> Result<(), ()> {
        self.count = self.count.checked_add(1).ok_or(())?;
        (self.count <= MAX_SCANNED_NODES).then_some(()).ok_or(())
    }
}

async fn is_target_process(
    dbus: &zbus::fdo::DBusProxy<'_>,
    reference: &atspi::ObjectRefOwned,
    target: &DiscoveryRequest,
    pids: &mut BTreeMap<String, u32>,
) -> Result<bool, ()> {
    let name = reference.name_as_str().ok_or(())?;
    let pid = if let Some(pid) = pids.get(name) {
        *pid
    } else {
        let pid = bus_pid(dbus, reference).await?;
        pids.insert(name.to_owned(), pid);
        pid
    };
    Ok(target.pids.contains(&pid))
}

async fn locate_projection_root(
    connection: &AccessibilityConnection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    target: &DiscoveryRequest,
    start: &atspi::ObjectRefOwned,
    wanted: u64,
    scan: &mut ProjectionScan,
) -> Result<atspi::ObjectRefOwned, ()> {
    let mut queue = VecDeque::from([(start.clone(), 0_u8)]);
    let mut visited = BTreeSet::new();
    while let Some((reference, depth)) = queue.pop_front() {
        if !is_target_process(dbus, &reference, target, &mut scan.pids).await? {
            continue;
        }
        let id = register_object(&reference, &mut scan.identities)?;
        if !visited.insert(id) {
            continue;
        }
        scan.inspect()?;
        if id == wanted {
            return Ok(reference);
        }
        if depth >= agent_semantic_helper::MAX_PROJECTED_DEPTH {
            continue;
        }
        let proxy = reference
            .as_accessible_proxy(connection.connection())
            .await
            .map_err(|_| ())?;
        let children = timeout(Duration::from_millis(CALL_MS), proxy.get_children())
            .await?
            .map_err(|_| ())?;
        let child_depth = depth.checked_add(1).ok_or(())?;
        queue.extend(children.into_iter().map(|child| (child, child_depth)));
    }
    Err(())
}

async fn projected_bounds(
    connection: &AccessibilityConnection,
    reference: &atspi::ObjectRefOwned,
    origin: &TargetRect,
) -> Option<TargetRect> {
    let proxy = reference
        .as_accessible_proxy(connection.connection())
        .await
        .ok()?;
    let proxies = timeout(Duration::from_millis(CALL_MS), proxy.proxies())
        .await
        .ok()?
        .ok()?;
    let component = timeout(Duration::from_millis(CALL_MS), proxies.component())
        .await
        .ok()?
        .ok()?;
    let (x, y, width, height) = timeout(
        Duration::from_millis(CALL_MS),
        component.get_extents(atspi::CoordType::Screen),
    )
    .await
    .ok()?
    .ok()?;
    let width = u16::try_from(width).ok()?;
    let height = u16::try_from(height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(TargetRect {
        x: x.checked_sub(origin.x)?,
        y: y.checked_sub(origin.y)?,
        width,
        height,
    })
}

fn projection_origin(target: &DiscoveryRequest, candidate: &Candidate) -> Option<TargetRect> {
    let content = *target.rects.first()?;
    if target.rects.contains(&candidate.rect) {
        return Some(content);
    }
    (target.single_client
        && target
            .rects
            .iter()
            .any(|rect| rect.width == candidate.rect.width && rect.height == candidate.rect.height))
    .then_some(candidate.rect)
}

async fn project_subtree(
    connection: &AccessibilityConnection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    target: &DiscoveryRequest,
    matched_root: &atspi::ObjectRefOwned,
    origin: &TargetRect,
    projection: ProjectionRequest,
) -> Result<(Vec<ProjectedNode>, Option<u16>), ()> {
    let mut scan = ProjectionScan::default();
    let subtree = if object_id(matched_root)? == projection.root {
        matched_root.clone()
    } else {
        locate_projection_root(
            connection,
            dbus,
            target,
            matched_root,
            projection.root,
            &mut scan,
        )
        .await?
    };
    let mut queue = VecDeque::from([(subtree, None, 0_u8)]);
    let mut visited = BTreeSet::new();
    let mut nodes = Vec::with_capacity(usize::from(projection.max_nodes));
    let mut position = 0_u16;
    while let Some((reference, parent, depth)) = queue.pop_front() {
        if !is_target_process(dbus, &reference, target, &mut scan.pids).await? {
            continue;
        }
        let id = register_object(&reference, &mut scan.identities)?;
        if !visited.insert(id) {
            continue;
        }
        scan.inspect()?;
        if position >= projection.offset && nodes.len() == usize::from(projection.max_nodes) {
            return Ok((nodes, Some(position)));
        }
        let proxy = reference
            .as_accessible_proxy(connection.connection())
            .await
            .map_err(|_| ())?;
        let children = timeout(Duration::from_millis(CALL_MS), proxy.get_children())
            .await?
            .map_err(|_| ())?;
        if depth < projection.max_depth {
            let child_depth = depth.checked_add(1).ok_or(())?;
            queue.extend(
                children
                    .iter()
                    .cloned()
                    .map(|child| (child, Some(id), child_depth)),
            );
        }
        if position >= projection.offset {
            let role = timeout(Duration::from_millis(CALL_MS), proxy.get_role())
                .await?
                .map_err(|_| ())?;
            let states = timeout(Duration::from_millis(CALL_MS), proxy.get_state())
                .await?
                .map_err(|_| ())?;
            let name = timeout(Duration::from_millis(CALL_MS), proxy.name())
                .await?
                .map_err(|_| ())?;
            nodes.push(ProjectedNode {
                id,
                parent,
                depth,
                role: projected_role(role),
                name: bounded_text(name),
                states: projected_states(&states, role),
                bounds: projected_bounds(connection, &reference, origin).await,
                child_count: u32::try_from(children.len()).map_err(|_| ())?,
            });
        }
        position = position.checked_add(1).ok_or(())?;
    }
    Ok((nodes, None))
}

fn search_matches(
    query: &SearchQuery,
    folded_name: Option<&str>,
    role: ProjectedRole,
    states: &[ProjectedState],
) -> bool {
    query
        .name
        .as_ref()
        .is_none_or(|name| folded_name.is_some_and(|candidate| candidate.contains(name)))
        && (query.roles.is_empty() || query.roles.contains(&role))
        && query.states.iter().all(|state| states.contains(state))
}

async fn search_subtree(
    connection: &AccessibilityConnection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    target: &DiscoveryRequest,
    matched_root: &atspi::ObjectRefOwned,
    origin: &TargetRect,
    search: &SearchRequest,
) -> Result<(Vec<ProjectedNode>, Option<u16>), ()> {
    let mut query = search.query.clone();
    query.name = query.name.map(|name| name.to_lowercase());
    let mut scan = ProjectionScan::default();
    let mut queue = VecDeque::from([(matched_root.clone(), None, 0_u8)]);
    let mut visited = BTreeSet::new();
    let mut nodes = Vec::with_capacity(usize::from(search.max_results));
    let mut position = 0_u16;
    while let Some((reference, parent, depth)) = queue.pop_front() {
        if !is_target_process(dbus, &reference, target, &mut scan.pids).await? {
            continue;
        }
        let id = register_object(&reference, &mut scan.identities)?;
        if !visited.insert(id) {
            continue;
        }
        scan.inspect()?;
        if position >= search.offset && nodes.len() == usize::from(search.max_results) {
            return Ok((nodes, Some(position)));
        }
        let proxy = reference
            .as_accessible_proxy(connection.connection())
            .await
            .map_err(|_| ())?;
        let children = timeout(Duration::from_millis(CALL_MS), proxy.get_children())
            .await?
            .map_err(|_| ())?;
        if depth < agent_semantic_helper::MAX_PROJECTED_DEPTH {
            let child_depth = depth.checked_add(1).ok_or(())?;
            queue.extend(
                children
                    .iter()
                    .cloned()
                    .map(|child| (child, Some(id), child_depth)),
            );
        }
        if position >= search.offset {
            let atspi_role = timeout(Duration::from_millis(CALL_MS), proxy.get_role())
                .await?
                .map_err(|_| ())?;
            let role = projected_role(atspi_role);
            let raw_states = timeout(Duration::from_millis(CALL_MS), proxy.get_state())
                .await?
                .map_err(|_| ())?;
            let states = projected_states(&raw_states, atspi_role);
            let name = bounded_text(
                timeout(Duration::from_millis(CALL_MS), proxy.name())
                    .await?
                    .map_err(|_| ())?,
            );
            let folded_name = name.as_ref().map(|name| name.to_lowercase());
            if search_matches(&query, folded_name.as_deref(), role, &states) {
                nodes.push(ProjectedNode {
                    id,
                    parent,
                    depth,
                    role,
                    name,
                    states,
                    bounds: projected_bounds(connection, &reference, origin).await,
                    child_count: u32::try_from(children.len()).map_err(|_| ())?,
                });
            }
        }
        position = position.checked_add(1).ok_or(())?;
    }
    Ok((nodes, None))
}

async fn discover_inner(target: &DiscoveryRequest) -> Result<DiscoveryResponse, ()> {
    let connection = AccessibilityConnection::new().await.map_err(|_| ())?;
    let root = connection
        .root_accessible_on_registry()
        .await
        .map_err(|_| ())?;
    let dbus = zbus::fdo::DBusProxy::new(connection.connection())
        .await
        .map_err(|_| ())?;
    apply_syscall_sandbox()?;
    let applications = timeout(Duration::from_millis(CALL_MS), root.get_children())
        .await?
        .map_err(|_| ())?;
    if applications.len() > MAX_APPLICATIONS {
        return Err(());
    }
    let mut discovered = Vec::new();
    for application in applications {
        let application_pid = bus_pid(&dbus, &application).await?;
        if !target.pids.contains(&application_pid) {
            continue;
        }
        let proxy = application
            .as_accessible_proxy(connection.connection())
            .await
            .map_err(|_| ())?;
        let top_levels = timeout(Duration::from_millis(CALL_MS), proxy.get_children())
            .await?
            .map_err(|_| ())?;
        if top_levels.len() > MAX_TOPLEVELS {
            return Err(());
        }
        for top_level in top_levels {
            let top_pid = bus_pid(&dbus, &top_level).await?;
            let proxy = top_level
                .as_accessible_proxy(connection.connection())
                .await
                .map_err(|_| ())?;
            let atspi_role = timeout(Duration::from_millis(CALL_MS), proxy.get_role())
                .await?
                .map_err(|_| ())?;
            let Some(role) = top_level_role(atspi_role) else {
                continue;
            };
            let states = timeout(Duration::from_millis(CALL_MS), proxy.get_state())
                .await?
                .map_err(|_| ())?;
            let proxies = timeout(Duration::from_millis(CALL_MS), proxy.proxies())
                .await?
                .map_err(|_| ())?;
            let component = timeout(Duration::from_millis(CALL_MS), proxies.component())
                .await?
                .map_err(|_| ())?;
            let (x, y, width, height) = timeout(
                Duration::from_millis(CALL_MS),
                component.get_extents(atspi::CoordType::Screen),
            )
            .await?
            .map_err(|_| ())?;
            let rect = TargetRect {
                x,
                y,
                width: u16::try_from(width).map_err(|_| ())?,
                height: u16::try_from(height).map_err(|_| ())?,
            };
            let candidate = Candidate {
                pids: if application_pid == top_pid {
                    vec![application_pid]
                } else {
                    vec![application_pid, top_pid]
                },
                rect,
                role,
                showing: states.contains(State::Showing),
                visible: states.contains(State::Visible),
                defunct: states.contains(State::Defunct),
            };
            discovered.push(Discovered {
                candidate,
                reference: top_level,
                role: atspi_role,
                states,
            });
            if discovered.len() > MAX_TOPLEVELS {
                return Err(());
            }
        }
    }
    let candidates = discovered
        .iter()
        .map(|candidate| candidate.candidate.clone())
        .collect::<Vec<_>>();
    let correlation = correlate_candidate(target, &candidates, true);
    let mut nodes = Vec::new();
    let mut next_offset = None;
    let root = match correlation {
        Correlation::Matched(index) => {
            let matched = discovered.get(index).ok_or(())?;
            let proxy = matched
                .reference
                .as_accessible_proxy(connection.connection())
                .await
                .map_err(|_| ())?;
            let name = timeout(Duration::from_millis(CALL_MS), proxy.name())
                .await?
                .map_err(|_| ())?;
            let child_count = timeout(Duration::from_millis(CALL_MS), proxy.child_count())
                .await?
                .map_err(|_| ())?;
            let origin = projection_origin(target, &matched.candidate).ok_or(())?;
            let id = object_id(&matched.reference)?;
            if let Some(projection) = target.projection {
                (nodes, next_offset) = project_subtree(
                    &connection,
                    &dbus,
                    target,
                    &matched.reference,
                    &origin,
                    projection,
                )
                .await?;
            } else if let Some(search) = target.search.as_ref() {
                (nodes, next_offset) = search_subtree(
                    &connection,
                    &dbus,
                    target,
                    &matched.reference,
                    &origin,
                    search,
                )
                .await?;
            }
            Some(RootProjection {
                id,
                role: match matched.candidate.role {
                    TopLevelRole::Dialog => ProjectedRole::Dialog,
                    TopLevelRole::Filler | TopLevelRole::Frame | TopLevelRole::Window => {
                        ProjectedRole::Window
                    }
                },
                name: bounded_text(name),
                states: projected_states(&matched.states, matched.role),
                bounds: TargetRect {
                    x: matched.candidate.rect.x.checked_sub(origin.x).ok_or(())?,
                    y: matched.candidate.rect.y.checked_sub(origin.y).ok_or(())?,
                    width: matched.candidate.rect.width,
                    height: matched.candidate.rect.height,
                },
                child_count: u32::try_from(child_count).map_err(|_| ())?,
            })
        }
        Correlation::Ambiguous | Correlation::Unavailable | Correlation::Invalid => None,
    };
    Ok(DiscoveryResponse {
        v: HELPER_VERSION,
        status: correlation.status(),
        root,
        nodes,
        next_offset,
    })
}

fn discover(target: &DiscoveryRequest) -> DiscoveryResponse {
    future::block_on(async {
        match timeout(
            Duration::from_millis(TOTAL_DISCOVERY_MS),
            discover_inner(target),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(())) | Err(()) => DiscoveryResponse {
                v: HELPER_VERSION,
                status: DiscoveryStatus::Unavailable,
                root: None,
                nodes: Vec::new(),
                next_offset: None,
            },
        }
    })
}

fn response(status: DiscoveryStatus) -> DiscoveryResponse {
    DiscoveryResponse {
        v: HELPER_VERSION,
        status,
        root: None,
        nodes: Vec::new(),
        next_offset: None,
    }
}

fn run(fixture: bool) -> DiscoveryResponse {
    if apply_process_limits().is_err() {
        return response(DiscoveryStatus::Unavailable);
    }
    let Ok(input) = read_input() else {
        return response(DiscoveryStatus::Invalid);
    };
    if fixture {
        let Ok(envelope) = serde_json::from_slice::<FixtureEnvelope>(&input) else {
            return response(DiscoveryStatus::Invalid);
        };
        if apply_syscall_sandbox().is_err() {
            return response(DiscoveryStatus::Unavailable);
        }
        return response(correlate(
            &envelope.target,
            &envelope.candidates,
            envelope.complete,
        ));
    }
    let Ok(target) = serde_json::from_slice::<DiscoveryRequest>(&input) else {
        return response(DiscoveryStatus::Invalid);
    };
    if target.validate().is_err() {
        return response(DiscoveryStatus::Invalid);
    }
    discover(&target)
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let fixture = matches!(arguments.next().as_deref(), Some(value) if value == "--fixture");
    if arguments.next().is_some() {
        return;
    }
    let response = run(fixture);
    let status = response.status;
    let mut stdout = io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &response).is_ok() {
        let _ = stdout.write_all(b"\n");
    }
    if status == DiscoveryStatus::Invalid {
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Candidate, DiscoveryRequest, HELPER_VERSION, ProjectedRole, ProjectedState, SearchQuery,
        TargetRect, TopLevelRole, projection_origin, search_matches,
    };

    fn candidate(rect: TargetRect) -> Candidate {
        Candidate {
            pids: vec![100],
            rect,
            role: TopLevelRole::Frame,
            showing: true,
            visible: true,
            defunct: false,
        }
    }

    fn target() -> DiscoveryRequest {
        DiscoveryRequest {
            v: HELPER_VERSION,
            pids: vec![100],
            rects: vec![
                TargetRect {
                    x: 240,
                    y: 100,
                    width: 800,
                    height: 600,
                },
                TargetRect {
                    x: 236,
                    y: 70,
                    width: 808,
                    height: 634,
                },
            ],
            single_client: true,
            projection: None,
            search: None,
        }
    }

    #[test]
    fn projection_origin_preserves_exact_screen_and_positionless_coordinates() {
        let target = target();
        assert_eq!(
            projection_origin(&target, &candidate(target.rects[1])),
            Some(target.rects[0])
        );
        let positionless = TargetRect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        assert_eq!(
            projection_origin(&target, &candidate(positionless)),
            Some(positionless)
        );
        assert_eq!(
            projection_origin(
                &target,
                &candidate(TargetRect {
                    width: 799,
                    ..positionless
                })
            ),
            None
        );
    }

    #[test]
    fn search_combines_name_role_and_state_predicates() {
        let query = SearchQuery {
            name: Some("continue".to_owned()),
            roles: vec![ProjectedRole::Button, ProjectedRole::Link],
            states: vec![ProjectedState::Focusable, ProjectedState::Visible],
        };
        assert!(search_matches(
            &query,
            Some("continue setup"),
            ProjectedRole::Button,
            &[ProjectedState::Focusable, ProjectedState::Visible],
        ));
        assert!(!search_matches(
            &query,
            Some("cancel"),
            ProjectedRole::Button,
            &[ProjectedState::Focusable, ProjectedState::Visible],
        ));
        assert!(!search_matches(
            &query,
            Some("continue setup"),
            ProjectedRole::Text,
            &[ProjectedState::Focusable, ProjectedState::Visible],
        ));
        assert!(!search_matches(
            &query,
            Some("continue setup"),
            ProjectedRole::Button,
            &[ProjectedState::Focusable],
        ));
    }
}
