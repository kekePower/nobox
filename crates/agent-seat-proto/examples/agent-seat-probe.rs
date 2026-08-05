//! A minimal Agent Seat Protocol client used by the integration tests.
//!
//! It doubles as the smallest complete example of speaking the protocol: find
//! the socket, greet, read the grant the manager actually issued, and act only
//! within it. Each scenario asserts the manager's answer itself, so the shell
//! around it only has to check an exit status.

use std::collections::BTreeSet;
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use agent_seat_proto::{
    Call, ClientId, ClientMessage, ErrorCode, Event, FrameLimits, Hello, Outcome, Reply, Request,
    RequestId, ServerMessage, Welcome, WorkspaceId, read_frame, write_frame,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let (Some(socket), Some(scenario)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: agent-seat-probe <socket> <scenario> [harness] [arguments...]");
        return ExitCode::FAILURE;
    };
    let harness = arguments.next().unwrap_or_else(|| "probe".to_owned());
    let rest: Vec<String> = arguments.collect();
    match run(&socket, &scenario, &harness, &rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("probe {scenario}: {failure}");
            ExitCode::FAILURE
        }
    }
}

struct Session {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    limits: FrameLimits,
    next_request: u64,
}

impl Session {
    fn connect(socket: &str) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|error| format!("cannot connect to {socket}: {error}"))?;
        let write_half = stream
            .try_clone()
            .map_err(|error| format!("cannot split the socket: {error}"))?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer: BufWriter::new(write_half),
            limits: FrameLimits::DEFAULT,
            next_request: 1,
        })
    }

    fn send(&mut self, message: &ClientMessage) -> Result<(), String> {
        write_frame(&mut self.writer, message, &self.limits)
            .map_err(|error| format!("cannot write a frame: {error}"))
    }

    fn receive(&mut self) -> Result<ServerMessage, String> {
        read_frame(&mut self.reader, &self.limits)
            .map_err(|error| format!("cannot read a frame: {error}"))
    }

    fn greet(&mut self, harness: &str) -> Result<Welcome, String> {
        let hello = Hello::new(harness, "agent seat integration test");
        self.send(&ClientMessage::Hello(hello))?;
        match self.receive()? {
            ServerMessage::Welcome(welcome) => Ok(welcome),
            other => Err(format!("expected a welcome, got {other:?}")),
        }
    }

    fn call(&mut self, call: Call) -> Result<Outcome, String> {
        let id = RequestId::new(self.next_request);
        self.next_request += 1;
        self.send(&ClientMessage::Request(Request { id, call }))?;
        match self.receive()? {
            ServerMessage::Response(response) if response.id == id => Ok(response.outcome),
            other => Err(format!("expected a response to {id}, got {other:?}")),
        }
    }

    fn subscribe(
        &mut self,
    ) -> Result<
        (
            Vec<agent_seat_proto::EventKind>,
            agent_seat_proto::DesktopSnapshot,
        ),
        String,
    > {
        match self.call(Call::SubscribeAndSnapshot { kinds: Vec::new() })? {
            Outcome::Ok {
                reply: Reply::Subscribed { kinds, snapshot },
            } => Ok((kinds, snapshot)),
            other => Err(format!("expected a subscription, got {other:?}")),
        }
    }

    fn set_timeout(&mut self, timeout: Option<Duration>) -> Result<(), std::io::Error> {
        self.reader.get_ref().set_read_timeout(timeout)
    }

    fn snapshot(&mut self) -> Result<agent_seat_proto::DesktopSnapshot, String> {
        match self.call(Call::DesktopSnapshot {})? {
            Outcome::Ok {
                reply: Reply::Snapshot { snapshot },
            } => Ok(snapshot),
            other => Err(format!("expected a snapshot, got {other:?}")),
        }
    }

    /// Returns the exact wire encoding of a refusal, so two refusals can be
    /// compared byte for byte rather than by their code alone.
    fn refusal(&mut self, client: ClientId) -> Result<String, String> {
        match self.call(Call::ClientGet { client })? {
            Outcome::Error { error } => {
                serde_json::to_string(&error).map_err(|error| error.to_string())
            }
            Outcome::Ok { .. } => Err(format!("{client} was unexpectedly readable")),
        }
    }

    fn expect_error(&mut self, call: Call, expected: ErrorCode) -> Result<(), String> {
        let tool = call.tool();
        let outcome = self.call(call)?;
        match outcome {
            Outcome::Error { error } if error.code == expected => {
                println!("{tool} -> {}", error.code.as_str());
                Ok(())
            }
            other => Err(format!(
                "expected {tool} to fail with {}, got {other:?}",
                expected.as_str()
            )),
        }
    }

    /// Reads until the manager closes the session, returning its goodbye.
    fn expect_goodbye(&mut self) -> Result<String, String> {
        loop {
            match self.receive() {
                Ok(ServerMessage::Goodbye(goodbye)) => {
                    println!("goodbye {:?}", goodbye.reason);
                    return Ok(goodbye.message);
                }
                Ok(_) => {}
                Err(error) => return Err(format!("expected a goodbye: {error}")),
            }
        }
    }

    /// Confirms the manager hung up, whether or not a goodbye arrived first.
    fn expect_closed(&mut self) -> Result<(), String> {
        let mut sink = Vec::new();
        match self.reader.read_to_end(&mut sink) {
            Ok(_) => {
                println!("closed");
                Ok(())
            }
            Err(error) => {
                println!("closed ({error})");
                Ok(())
            }
        }
    }
}

fn run(socket: &str, scenario: &str, harness: &str, arguments: &[String]) -> Result<(), String> {
    match scenario {
        "granted" => granted(socket, harness),
        "snapshot" => snapshot(socket, harness),
        "watch" => watch(socket, harness, arguments),
        "hidden-oracle" => hidden_oracle(socket, harness, arguments),
        "unbound" => unbound(socket, harness),
        "version" => version(socket, harness),
        "no-hello" => no_hello(socket),
        "second-hello" => second_hello(socket, harness),
        "oversize" => oversize(socket),
        "garbage" => garbage(socket),
        "truncate" => truncate(socket),
        "flood" => flood(socket, harness),
        other => Err(format!("unknown scenario {other}")),
    }
}

/// A companion whose executable a stored grant names holds exactly the atoms
/// that grant lists, and nothing beside them.
fn granted(socket: &str, harness: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    let welcome = session.greet(harness)?;
    let atoms: Vec<&str> = welcome
        .granted
        .atoms()
        .into_iter()
        .map(agent_seat_proto::Capability::as_str)
        .collect();
    println!("welcome granted={}", atoms.join(","));
    if atoms != ["observe.structure", "observe.titles"] {
        return Err(format!("unexpected grant {atoms:?}"));
    }
    if welcome.scoped {
        return Err("the grant should not be scoped".to_owned());
    }
    // Granted: the call is answered rather than refused.
    session.snapshot()?;
    // Granted, but naming nothing: a refusal about the object, not the grant.
    session.expect_error(
        Call::ClientGet {
            client: ClientId::new(0xffff_fff0),
        },
        ErrorCode::NoSuchClient,
    )?;
    // Not granted: observe never implies manage, and the manager says so.
    session.expect_error(
        Call::WorkspaceSwitch {
            workspace: WorkspaceId::new(1),
        },
        ErrorCode::Denied,
    )?;
    Ok(())
}

/// The same declared identity from an executable no grant names holds nothing.
/// Declared strings are display text, never an authorization input.
fn unbound(socket: &str, harness: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    let welcome = session.greet(harness)?;
    println!("welcome granted={}", welcome.granted.atoms().len());
    if !welcome.granted.is_empty() {
        return Err(format!(
            "an unbound executable was granted {:?}",
            welcome.granted.atoms()
        ));
    }
    session.expect_error(Call::DesktopSnapshot {}, ErrorCode::Denied)?;
    // A session with no grant must not learn anything from errors either.
    session.expect_error(
        Call::ClientGet {
            client: ClientId::new(1),
        },
        ErrorCode::Denied,
    )?;
    Ok(())
}

/// The structured world model an agent works from instead of screenshots.
fn snapshot(socket: &str, harness: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    session.greet(harness)?;
    let snapshot = session.snapshot()?;
    println!("sequence {}", snapshot.sequence);
    for client in &snapshot.clients {
        println!(
            "client {} class={} title={}",
            client.client,
            client.application.class.as_deref().unwrap_or("-"),
            client.title.as_deref().unwrap_or("-")
        );
    }
    println!("workspaces {}", snapshot.workspaces.len());
    println!("outputs {}", snapshot.outputs.len());
    if snapshot.clients.is_empty() {
        return Err("the snapshot contains no windows at all".to_owned());
    }
    if snapshot.clients.len() != snapshot.stacking.len() {
        return Err("stacking order and descriptors disagree".to_owned());
    }
    // Every descriptor must answer identically on its own.
    for client in &snapshot.clients {
        let outcome = session.call(Call::ClientGet {
            client: client.client,
        })?;
        match outcome {
            Outcome::Ok {
                reply: Reply::Client { client: fetched },
            } if fetched.client == client.client => {}
            other => return Err(format!("client.get disagreed with the snapshot: {other:?}")),
        }
    }
    Ok(())
}

/// Subscribes and follows the stream through a window appearing and going
/// away, checking the properties an agent's world model depends on.
fn watch(socket: &str, harness: &str, arguments: &[String]) -> Result<(), String> {
    let expected = arguments
        .first()
        .ok_or_else(|| "watch needs the title to wait for".to_owned())?;
    let mut session = Session::connect(socket)?;
    let welcome = session.greet(harness)?;
    let (kinds, snapshot) = session.subscribe()?;
    println!(
        "subscribed sequence={} clients={} kinds={} scoped={}",
        snapshot.sequence,
        snapshot.clients.len(),
        kinds.len(),
        welcome.scoped
    );
    let mut known: BTreeSet<u64> = snapshot
        .clients
        .iter()
        .map(|client| client.client.raw())
        .collect();
    // The snapshot and the stream are one operation: events continue from the
    // snapshot's sequence, never from before it.
    let mut last = snapshot.sequence;
    let mut watched: Option<u64> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    session
        .set_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("cannot bound the watch: {error}"))?;
    while Instant::now() < deadline {
        let envelope = match session.receive() {
            Ok(ServerMessage::Event(envelope)) => envelope,
            Ok(_) => continue,
            Err(error) if error.contains("timed out") || error.contains("blocking") => continue,
            Err(error) => return Err(error),
        };
        if envelope.sequence.raw() <= last.raw() {
            return Err(format!(
                "sequence {} did not advance past {last}",
                envelope.sequence
            ));
        }
        last = envelope.sequence;
        let subject = event_subject(&envelope.event);
        println!("event {} {:?}", envelope.sequence, envelope.event.kind());
        match &envelope.event {
            Event::ClientMapped { client, .. } => {
                println!(
                    "mapped {} title={}",
                    client.client,
                    client.title.as_deref().unwrap_or("-")
                );
                known.insert(client.client.raw());
                if client.title.as_deref() == Some(expected.as_str()) {
                    watched = Some(client.client.raw());
                }
            }
            Event::ClientClosed { client } => {
                known.remove(&client.raw());
                if watched == Some(client.raw()) {
                    println!("watched window appeared and went away");
                    return Ok(());
                }
            }
            Event::ResyncRequired { dropped } => {
                return Err(format!("the stream overflowed, dropping {dropped}"));
            }
            _ => {}
        }
        // A scoped session must never learn that anything outside its scope
        // exists, through events any more than through snapshots.
        if welcome.scoped
            && let Some(subject) = subject
            && !known.contains(&subject)
        {
            return Err(format!("a scoped session saw an event about {subject}"));
        }
    }
    Err(format!(
        "no window titled {expected} appeared and closed in time"
    ))
}

fn event_subject(event: &Event) -> Option<u64> {
    match event {
        Event::ClientMapped { client, .. } => Some(client.client.raw()),
        Event::ClientClosed { client }
        | Event::TitleChanged { client, .. }
        | Event::StateChanged { client, .. }
        | Event::GeometryChanged { client, .. } => Some(client.raw()),
        Event::FocusChanged { client } => client.map(ClientId::raw),
        Event::WorkspaceSwitched { .. }
        | Event::HumanActivity { .. }
        | Event::SessionControl { .. }
        | Event::ResyncRequired { .. } => None,
    }
}

/// Windows a rule hides must be indistinguishable from windows that do not
/// exist — in the snapshot and in every error.
fn hidden_oracle(socket: &str, harness: &str, managed: &[String]) -> Result<(), String> {
    if managed.is_empty() {
        return Err("no managed window identities were supplied".to_owned());
    }
    let mut session = Session::connect(socket)?;
    session.greet(harness)?;
    let snapshot = session.snapshot()?;
    let visible: Vec<u64> = snapshot
        .clients
        .iter()
        .map(|client| client.client.raw())
        .collect();
    let nonexistent = session.refusal(ClientId::new(0xffff_fff0))?;
    let mut withheld = 0;
    for identity in managed {
        let raw = identity
            .trim_start_matches("0x")
            .parse::<u64>()
            .or_else(|_| u64::from_str_radix(identity.trim_start_matches("0x"), 16))
            .map_err(|error| format!("unusable window identity {identity}: {error}"))?;
        if visible.contains(&raw) {
            continue;
        }
        withheld += 1;
        let refusal = session.refusal(ClientId::new(raw))?;
        if refusal != nonexistent {
            return Err(format!(
                "a withheld window answered {refusal} where a nonexistent one answered {nonexistent}"
            ));
        }
        println!("withheld {raw} -> {refusal}");
    }
    if withheld == 0 {
        return Err("every managed window was visible; the test proved nothing".to_owned());
    }
    println!("withheld {withheld} of {} managed windows", managed.len());
    Ok(())
}

fn version(socket: &str, harness: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    let mut hello = Hello::new(harness, "version mismatch");
    hello.version = agent_seat_proto::PROTOCOL_VERSION + 1;
    session.send(&ClientMessage::Hello(hello))?;
    session.expect_goodbye()?;
    Ok(())
}

fn no_hello(socket: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    session.send(&ClientMessage::Request(Request {
        id: RequestId::new(1),
        call: Call::DesktopSnapshot {},
    }))?;
    session.expect_goodbye()?;
    Ok(())
}

fn second_hello(socket: &str, harness: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    session.greet(harness)?;
    session.send(&ClientMessage::Hello(Hello::new(harness, "again")))?;
    session.expect_goodbye()?;
    Ok(())
}

/// A length prefix far above the protocol's bound must be refused before the
/// manager allocates anything for it.
fn oversize(socket: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    session
        .writer
        .write_all(&u32::MAX.to_be_bytes())
        .and_then(|()| session.writer.write_all(&[b'{'; 64]))
        .and_then(|()| session.writer.flush())
        .map_err(|error| format!("cannot write: {error}"))?;
    session.expect_closed()
}

fn garbage(socket: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    let body = b"this is not a protocol frame";
    let length = u32::try_from(body.len()).map_err(|error| error.to_string())?;
    session
        .writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| session.writer.write_all(body))
        .and_then(|()| session.writer.flush())
        .map_err(|error| format!("cannot write: {error}"))?;
    session.expect_closed()
}

/// Announce a frame, send half of it, and vanish.
fn truncate(socket: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    session
        .writer
        .write_all(&256_u32.to_be_bytes())
        .and_then(|()| session.writer.write_all(&[b' '; 128]))
        .and_then(|()| session.writer.flush())
        .map_err(|error| format!("cannot write: {error}"))?;
    println!("abandoned");
    Ok(())
}

/// Write as fast as possible and never read. The manager must shed this
/// session rather than slow down.
fn flood(socket: &str, harness: &str) -> Result<(), String> {
    let mut session = Session::connect(socket)?;
    session.send(&ClientMessage::Hello(Hello::new(harness, "flood")))?;
    let mut sent = 0_u32;
    for id in 1..=4096_u64 {
        let request = ClientMessage::Request(Request {
            id: RequestId::new(id),
            call: Call::DesktopSnapshot {},
        });
        if session.send(&request).is_err() {
            break;
        }
        sent += 1;
    }
    println!("flooded {sent}");
    Ok(())
}
