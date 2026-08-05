use std::{
    env,
    ffi::OsString,
    io::{BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use nobox_x11::ControlSender;
use tracing::{debug, info, warn};

const MAX_MESSAGE_BYTES: usize = 4_096;
const MAX_CLIENT_ID_BYTES: usize = 1_024;

pub(crate) struct XsmpBridge {
    child: Child,
    input: Arc<Mutex<ChildStdin>>,
    reader: Option<JoinHandle<()>>,
    client_id: Arc<Mutex<Option<String>>>,
    closed: bool,
}

impl XsmpBridge {
    pub(crate) fn requested() -> bool {
        env::var_os("SESSION_MANAGER").is_some_and(|value| !value.is_empty())
    }

    pub(crate) fn connect(control: ControlSender, requested_id: Option<&str>) -> Option<Self> {
        if !Self::requested() {
            return None;
        }
        let helper = env::var_os("NOBOX_XSMP_HELPER")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("nobox-xsmp"));
        let mut command = Command::new(&helper);
        if let Some(requested_id) = requested_id {
            command.arg("--client-id").arg(requested_id);
        }
        command
            .arg("--")
            .args(restart_arguments())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                warn!(
                    helper = ?helper,
                    %error,
                    "SESSION_MANAGER is set but the optional XSMP helper could not start"
                );
                return None;
            }
        };
        let Some(input) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            warn!("XSMP helper did not expose its command input");
            return None;
        };
        let Some(output) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            warn!("XSMP helper did not expose its event output");
            return None;
        };
        let input = Arc::new(Mutex::new(input));
        let client_id = Arc::new(Mutex::new(None));
        let reader_input = Arc::clone(&input);
        let reader_client_id = Arc::clone(&client_id);
        let reader = match thread::Builder::new()
            .name("nobox-xsmp".to_owned())
            .spawn(move || {
                xsmp_events(
                    BufReader::new(output),
                    control,
                    &reader_input,
                    &reader_client_id,
                );
            }) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                warn!(%error, "could not start the XSMP event reader");
                return None;
            }
        };
        Some(Self {
            child,
            input,
            reader: Some(reader),
            client_id,
            closed: false,
        })
    }

    pub(crate) fn save_done(&self, success: bool) {
        if let Err(error) = write_command(
            &self.input,
            if success {
                "SAVE_DONE\t1"
            } else {
                "SAVE_DONE\t0"
            },
        ) {
            warn!(%error, "could not acknowledge the XSMP save request");
        }
    }

    pub(crate) fn request_logout(&self) -> bool {
        match write_command(&self.input, "LOGOUT") {
            Ok(()) => true,
            Err(error) => {
                warn!(%error, "could not request logout from the XSMP session manager");
                false
            }
        }
    }

    pub(crate) fn client_id(&self) -> Option<String> {
        self.client_id.lock().ok().and_then(|id| id.clone())
    }

    pub(crate) fn close(mut self, permanent: bool) {
        self.close_inner(permanent);
    }

    fn close_inner(&mut self, permanent: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Err(error) =
            write_command(&self.input, if permanent { "CLOSE\t1" } else { "CLOSE\t0" })
        {
            debug!(%error, "XSMP helper command input was already closed");
        }
        for _ in 0..50 {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    warn!(%error, "could not inspect the XSMP helper");
                    break;
                }
            }
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take()
            && reader.join().is_err()
        {
            warn!("XSMP event reader panicked");
        }
    }
}

impl Drop for XsmpBridge {
    fn drop(&mut self) {
        self.close_inner(false);
    }
}

fn restart_arguments() -> Vec<OsString> {
    let mut arguments = env::args_os();
    let mut filtered = Vec::new();
    while let Some(argument) = arguments.next() {
        if argument == "--sm-client-id" {
            let _ = arguments.next();
        } else if !argument.to_string_lossy().starts_with("--sm-client-id=") {
            filtered.push(argument);
        }
    }
    filtered
}

fn xsmp_events(
    mut output: BufReader<impl Read>,
    control: ControlSender,
    input: &Arc<Mutex<ChildStdin>>,
    client_id: &Arc<Mutex<Option<String>>>,
) {
    while let Some(message) = read_message(&mut output) {
        match message.as_str() {
            "SAVE" => {
                if let Err(error) = control.save_session() {
                    warn!(%error, "could not route an XSMP save request");
                    let _ = write_command(input, "SAVE_DONE\t0");
                }
            }
            "DIE" => {
                info!("XSMP session manager requested shutdown");
                if let Err(error) = control.shutdown() {
                    warn!(%error, "could not route an XSMP shutdown request");
                }
            }
            "SAVE_COMPLETE" => debug!("XSMP session save completed"),
            "SHUTDOWN_CANCELLED" => debug!("XSMP shutdown was cancelled"),
            _ if message.starts_with("CONNECTED\t") => {
                let id = &message[10..];
                if !id.is_empty()
                    && id.len() <= MAX_CLIENT_ID_BYTES
                    && !id.chars().any(char::is_control)
                {
                    if let Ok(mut current) = client_id.lock() {
                        *current = Some(id.to_owned());
                    }
                    info!(client_id = id, "connected to the XSMP session manager");
                } else {
                    warn!("XSMP helper returned an invalid client id");
                }
            }
            _ => warn!(%message, "ignored an unknown XSMP helper message"),
        }
    }
}

fn read_message(reader: &mut BufReader<impl Read>) -> Option<String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    let mut oversized = false;
    loop {
        match reader.read(&mut byte) {
            Ok(0) if bytes.is_empty() => return None,
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if bytes.len() < MAX_MESSAGE_BYTES => bytes.push(byte[0]),
            Ok(_) => oversized = true,
            Err(error) => {
                warn!(%error, "could not read XSMP helper output");
                return None;
            }
        }
    }
    if oversized {
        warn!("discarded an oversized XSMP helper message");
        return Some(String::new());
    }
    match String::from_utf8(bytes) {
        Ok(message) => Some(message.trim_end_matches('\r').to_owned()),
        Err(_) => {
            warn!("discarded non-UTF-8 XSMP helper output");
            Some(String::new())
        }
    }
}

fn write_command(input: &Arc<Mutex<ChildStdin>>, command: &str) -> std::io::Result<()> {
    let mut input = input
        .lock()
        .map_err(|_| std::io::Error::other("XSMP helper input lock was poisoned"))?;
    writeln!(input, "{command}")?;
    input.flush()
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::{MAX_MESSAGE_BYTES, read_message};

    #[test]
    fn helper_messages_are_utf8_and_strictly_bounded() {
        let mut ordinary = BufReader::new(Cursor::new(b"SAVE\r\nDIE"));
        assert_eq!(read_message(&mut ordinary).as_deref(), Some("SAVE"));
        assert_eq!(read_message(&mut ordinary).as_deref(), Some("DIE"));
        assert_eq!(read_message(&mut ordinary), None);

        let exact = vec![b'x'; MAX_MESSAGE_BYTES];
        assert_eq!(
            read_message(&mut BufReader::new(Cursor::new(exact)))
                .expect("exact limit is accepted")
                .len(),
            MAX_MESSAGE_BYTES
        );
        let oversized = vec![b'x'; MAX_MESSAGE_BYTES.saturating_add(1)];
        assert_eq!(
            read_message(&mut BufReader::new(Cursor::new(oversized))).as_deref(),
            Some("")
        );
        assert_eq!(
            read_message(&mut BufReader::new(Cursor::new([0xff]))).as_deref(),
            Some("")
        );
    }
}
