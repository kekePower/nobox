//! One-shot bounded AT-SPI root correlation process.

use std::future::Future;
use std::io::{self, Read, Write};
use std::time::Duration;
use std::{collections::BTreeMap, convert::TryInto};

use agent_semantic_helper::{
    Candidate, Correlation, DiscoveryRequest, DiscoveryResponse, DiscoveryStatus, HELPER_VERSION,
    MAX_APPLICATIONS, MAX_INPUT_BYTES, MAX_TOPLEVELS, ProjectedRole, ProjectedState,
    RootProjection, TargetRect, TopLevelRole, correlate, correlate_candidate,
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

fn projected_states(states: &atspi::StateSet) -> Vec<ProjectedState> {
    let mut projected = Vec::new();
    if states.contains(State::Active) {
        projected.push(ProjectedState::Active);
    }
    if states.contains(State::Busy) {
        projected.push(ProjectedState::Busy);
    }
    if !states.contains(State::Enabled) {
        projected.push(ProjectedState::Disabled);
    }
    if states.contains(State::Focusable) {
        projected.push(ProjectedState::Focusable);
    }
    if states.contains(State::Focused) {
        projected.push(ProjectedState::Focused);
    }
    if states.contains(State::Modal) {
        projected.push(ProjectedState::Modal);
    }
    if states.contains(State::Visible) {
        projected.push(ProjectedState::Visible);
    }
    projected
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
            let role = timeout(Duration::from_millis(CALL_MS), proxy.get_role())
                .await?
                .map_err(|_| ())?;
            let Some(role) = top_level_role(role) else {
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
            let origin = target.rects.first().ok_or(())?;
            Some(RootProjection {
                role: match matched.candidate.role {
                    TopLevelRole::Dialog => ProjectedRole::Dialog,
                    TopLevelRole::Filler | TopLevelRole::Frame | TopLevelRole::Window => {
                        ProjectedRole::Window
                    }
                },
                name: bounded_text(name),
                states: projected_states(&matched.states),
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
            },
        }
    })
}

fn response(status: DiscoveryStatus) -> DiscoveryResponse {
    DiscoveryResponse {
        v: HELPER_VERSION,
        status,
        root: None,
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
