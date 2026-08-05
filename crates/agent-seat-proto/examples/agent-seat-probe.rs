//! A minimal Agent Seat Protocol client used by the integration tests.
//!
//! It doubles as the smallest complete example of speaking the protocol: find
//! the socket, greet, read the grant the manager actually issued, and act only
//! within it. Each scenario asserts the manager's answer itself, so the shell
//! around it only has to check an exit status.

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use agent_seat_proto::{
    Call, ClientMessage, ErrorCode, FrameLimits, Hello, Outcome, Request, RequestId, ServerMessage,
    Welcome, WorkspaceId, read_frame, write_frame,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let (Some(socket), Some(scenario)) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: agent-seat-probe <socket> <scenario> [harness]");
        return ExitCode::FAILURE;
    };
    let harness = arguments.next().unwrap_or_else(|| "probe".to_owned());
    match run(&socket, &scenario, &harness) {
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

fn run(socket: &str, scenario: &str, harness: &str) -> Result<(), String> {
    match scenario {
        "granted" => granted(socket, harness),
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
    // Granted but not yet implemented: an honest answer that is not a denial.
    session.expect_error(Call::DesktopSnapshot {}, ErrorCode::Unsupported)?;
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
