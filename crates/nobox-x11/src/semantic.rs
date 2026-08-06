//! Disposable accessibility-helper process lifecycle.

use std::env;
use std::fs::{self, DirBuilder};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{CONTROL_AGENT_SEMANTIC_READY, ControlSender};

const HELPER_VERSION: u8 = 1;
const MAX_OUTPUT_BYTES: u64 = 4 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const HARD_DEADLINE: Duration = Duration::from_millis(1_100);

/// One X11 screen-coordinate rectangle passed to the helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

/// Server-verified evidence for one authorized client.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Request {
    v: u8,
    pids: Vec<u32>,
    rects: Vec<Rect>,
    single_client: bool,
}

impl Request {
    pub(crate) fn new(pid: u32, rects: Vec<Rect>, single_client: bool) -> Option<Self> {
        if pid == 0 || rects.is_empty() || rects.len() > 2 {
            return None;
        }
        Some(Self {
            v: HELPER_VERSION,
            pids: vec![pid],
            rects,
            single_client,
        })
    }
}

/// The only distinction the manager retains from helper output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Result {
    Matched(Root),
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Completed {
    pub(crate) generation: u32,
    pub(crate) result: Result,
}

enum RunnerCommand {
    Start { generation: u32, request: Request },
    Cancel(u32),
    Stop,
}

/// One worker thread owns at most one disposable helper process globally.
pub(crate) struct Runner {
    commands: Sender<RunnerCommand>,
    completed: Receiver<Completed>,
    thread: Option<JoinHandle<()>>,
}

impl Runner {
    pub(crate) fn spawn(control: ControlSender) -> std::io::Result<Self> {
        let helper = helper_path();
        let (commands, receiver) = mpsc::channel();
        let (completed_sender, completed) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("nobox-semantic-runner".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || run(receiver, completed_sender, control, helper))?;
        Ok(Self {
            commands,
            completed,
            thread: Some(thread),
        })
    }

    pub(crate) fn start(&self, generation: u32, request: Request) -> bool {
        self.commands
            .send(RunnerCommand::Start {
                generation,
                request,
            })
            .is_ok()
    }

    pub(crate) fn cancel(&self, generation: u32) {
        let _ = self.commands.send(RunnerCommand::Cancel(generation));
    }

    pub(crate) fn take_completed(&self) -> Vec<Completed> {
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

fn run(
    commands: Receiver<RunnerCommand>,
    completed: Sender<Completed>,
    control: ControlSender,
    helper: Option<PathBuf>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            RunnerCommand::Start {
                generation,
                request,
            } => {
                let Some(helper) = helper.as_deref() else {
                    complete(&completed, &control, generation, Result::Unavailable);
                    continue;
                };
                if !run_one(
                    &commands, &completed, &control, helper, generation, &request,
                ) {
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
    control: &ControlSender,
    helper: &Path,
    generation: u32,
    request: &Request,
) -> bool {
    let Some(private_dir) = private_directory(generation) else {
        complete(completed, control, generation, Result::Unavailable);
        return true;
    };
    let outcome = spawn_helper(helper, &private_dir, request);
    let (mut child, output_thread) = match outcome {
        Ok(value) => value,
        Err(()) => {
            let _ = fs::remove_dir(&private_dir);
            complete(completed, control, generation, Result::Unavailable);
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
            }) => complete(completed, control, generation, Result::Unavailable),
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
        complete(completed, control, generation, result);
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
    control: &ControlSender,
    generation: u32,
    result: Result,
) {
    if completed.send(Completed { generation, result }).is_ok()
        && let Err(error) = control.send_data(CONTROL_AGENT_SEMANTIC_READY, generation)
    {
        warn!(%error, "could not deliver semantic helper completion");
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    v: u8,
    status: WireStatus,
    #[serde(default)]
    root: Option<Root>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Root {
    pub(crate) role: agent_seat_proto::SemanticRole,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) states: Vec<agent_seat_proto::SemanticState>,
    pub(crate) bounds: agent_seat_proto::Rect,
    pub(crate) child_count: u32,
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
        }) => Result::Matched(root),
        Ok(WireResponse {
            status: WireStatus::Ambiguous | WireStatus::Unavailable | WireStatus::Invalid,
            ..
        })
        | Ok(_)
        | Err(_) => Result::Unavailable,
    }
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
    use super::{Rect, Request, Result, parse_output};

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
            parse_output(b"{\"v\":1,\"status\":\"matched\",\"root\":{\"role\":\"window\",\"name\":\"Demo\",\"states\":[\"visible\"],\"bounds\":{\"x\":0,\"y\":0,\"width\":900,\"height\":600},\"child_count\":2}}\n"),
            Result::Matched(super::Root {
                role: agent_seat_proto::SemanticRole::Window,
                name: Some("Demo".to_owned()),
                states: vec![agent_seat_proto::SemanticState::Visible],
                bounds: agent_seat_proto::Rect::new(0, 0, 900, 600),
                child_count: 2,
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
        assert_eq!(parse_output(&vec![b' '; 4 * 1024 + 1]), Result::Unavailable);
    }
}
