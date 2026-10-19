//! Legacy AFNI plugout TCP transport (feature `talk`).
//!
//! AFNI's plugout protocol uses two TCP connections. A client first connects
//! to `AFNI_PLUGOUT_TCP_0` and sends a NUL-terminated registration containing
//! `PONAME` and `IOCHAN`. The receiver opens the requested data listener,
//! replies with a four-byte acknowledgement, and the client then connects to
//! that listener. Data messages and acknowledgements are also NUL/four-byte
//! framed.
//!
//! This module implements the portable TCP form used by `plugout_drive`.
//! Shared-memory `IOCHAN` specifications can be represented and parsed, but
//! [`Server`] rejects them explicitly instead of silently changing semantics.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::thread;
use std::time::{Duration, Instant};

use crate::talk::{Ports, AFNI_PLUGOUT_TCP_0, PLUGOUT_DRIVE_PORT};

/// AFNI's control-channel buffer size.
pub const DEFAULT_MAX_REGISTRATION: usize = 16 * 1024;
/// AFNI's plugout data buffer size.
pub const DEFAULT_MAX_MESSAGE: usize = 64 * 1024;
/// Maximum command text accepted by the official `plugout_drive` program.
pub const DRIVE_AFNI_COMMAND_LIMIT: usize = 4 * 1024;
/// Prefix placed before a command sent through the driver plugout.
pub const DRIVE_AFNI_PREFIX: &str = "DRIVE_AFNI ";
/// Size of every legacy plugout acknowledgement, including its trailing NUL.
pub const ACK_SIZE: usize = 4;
/// Successful legacy acknowledgement.
pub const ACK_OK: [u8; ACK_SIZE] = *b"OK!\0";
/// Failed legacy acknowledgement.
pub const ACK_BAD: [u8; ACK_SIZE] = *b"BAD\0";

/// Protocol stage associated with an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Connecting to or accepting the control channel.
    ControlConnect,
    /// Sending or receiving the registration string.
    Registration,
    /// Sending or receiving the registration acknowledgement.
    RegistrationAck,
    /// Connecting to, binding, or accepting the data channel.
    DataConnect,
    /// Sending or receiving a data message.
    Data,
    /// Sending or receiving a data acknowledgement.
    DataAck,
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ControlConnect => "control connection",
            Self::Registration => "registration",
            Self::RegistrationAck => "registration acknowledgement",
            Self::DataConnect => "data connection",
            Self::Data => "data message",
            Self::DataAck => "data acknowledgement",
        };
        formatter.write_str(name)
    }
}

/// Errors from the legacy plugout transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A socket operation failed during a known protocol phase.
    #[error("plugout {phase} I/O error: {source}")]
    Io {
        /// The active protocol phase.
        phase: Phase,
        /// The underlying socket error.
        source: std::io::Error,
    },
    /// A bounded operation did not complete in time.
    #[error("plugout {phase} timed out after {timeout:?}")]
    Timeout {
        /// The active protocol phase.
        phase: Phase,
        /// Configured deadline.
        timeout: Duration,
    },
    /// The peer closed a partially received frame.
    #[error("plugout peer closed during {phase} with {received} unterminated bytes")]
    PrematureClose {
        /// The active protocol phase.
        phase: Phase,
        /// Bytes received before EOF.
        received: usize,
    },
    /// A NUL-terminated frame exceeded its configured cap.
    #[error("plugout {phase} exceeded the {limit}-byte limit")]
    FrameTooLarge {
        /// The active protocol phase.
        phase: Phase,
        /// Maximum permitted payload length.
        limit: usize,
    },
    /// Registration text was not valid or lacked a required directive.
    #[error("malformed plugout registration: {0}")]
    MalformedRegistration(String),
    /// A data message was not valid UTF-8.
    #[error("plugout {phase} is not valid UTF-8: {source}")]
    Utf8 {
        /// The active protocol phase.
        phase: Phase,
        /// UTF-8 decoding failure.
        source: std::string::FromUtf8Error,
    },
    /// A string contained a NUL that would terminate its frame early.
    #[error("plugout {field} contains an embedded NUL")]
    EmbeddedNul {
        /// Name of the invalid input.
        field: &'static str,
    },
    /// The peer sent four bytes that are not an AFNI acknowledgement.
    #[error("invalid plugout acknowledgement {0:?}")]
    InvalidAcknowledgement([u8; ACK_SIZE]),
    /// The caller used a session operation out of order.
    #[error("invalid plugout state: {0}")]
    InvalidState(&'static str),
    /// A parsed channel is understood but unavailable in this implementation.
    #[error("unsupported plugout channel {0:?}")]
    UnsupportedChannel(String),
    /// Address resolution produced no candidates.
    #[error("plugout address {0:?} resolved to no socket addresses")]
    NoAddresses(String),
}

/// Result type for plugout operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Four-byte response used for registration and commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acknowledgement {
    /// The registration or command was accepted.
    Ok,
    /// The registration or command was rejected.
    Bad,
}

impl Acknowledgement {
    /// Exact four-byte legacy representation.
    pub const fn as_bytes(self) -> &'static [u8; ACK_SIZE] {
        match self {
            Self::Ok => &ACK_OK,
            Self::Bad => &ACK_BAD,
        }
    }

    /// Parse an exact legacy acknowledgement.
    pub fn from_bytes(bytes: [u8; ACK_SIZE]) -> Result<Self> {
        match bytes {
            ACK_OK => Ok(Self::Ok),
            ACK_BAD => Ok(Self::Bad),
            other => Err(Error::InvalidAcknowledgement(other)),
        }
    }
}

/// Data-channel specification carried by an AFNI registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    /// Portable TCP channel (`tcp:host:port`).
    Tcp {
        /// Host the client will connect to after registration.
        host: String,
        /// Data port selected by the client.
        port: u16,
    },
    /// Legacy Unix shared-memory channel. It is parsed but not implemented.
    SharedMemory(String),
}

impl Channel {
    fn parse(specification: &str) -> Result<Self> {
        if let Some(rest) = specification.strip_prefix("tcp:") {
            let (host, port) = rest.rsplit_once(':').ok_or_else(|| {
                Error::MalformedRegistration(format!(
                    "invalid TCP IOCHAN specification {specification:?}"
                ))
            })?;
            if host.is_empty() || host.chars().any(char::is_whitespace) {
                return Err(Error::MalformedRegistration(format!(
                    "invalid TCP host in {specification:?}"
                )));
            }
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| {
                    Error::MalformedRegistration(format!("invalid TCP port in {specification:?}"))
                })?;
            Ok(Self::Tcp {
                host: host.to_owned(),
                port,
            })
        } else if specification.starts_with("shm:") {
            Ok(Self::SharedMemory(specification.to_owned()))
        } else {
            Err(Error::MalformedRegistration(format!(
                "unknown IOCHAN specification {specification:?}"
            )))
        }
    }

    fn specification(&self) -> String {
        match self {
            Self::Tcp { host, port } => format!("tcp:{host}:{port}"),
            Self::SharedMemory(specification) => specification.clone(),
        }
    }
}

/// Parsed plugout control registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Informational plugout name shown by AFNI.
    pub name: String,
    /// Requested data channel.
    pub channel: Channel,
    /// Whether the peer expects four-byte acknowledgements.
    pub acknowledgements: bool,
    /// Other legal/extension directives, preserved in their original order.
    pub directives: Vec<String>,
}

impl Registration {
    /// Build the registration used by a TCP `plugout_drive` client.
    pub fn tcp(name: impl Into<String>, host: impl Into<String>, port: u16) -> Result<Self> {
        let registration = Self {
            name: name.into(),
            channel: Channel::Tcp {
                host: host.into(),
                port,
            },
            acknowledgements: true,
            directives: Vec::new(),
        };
        registration.validate()?;
        Ok(registration)
    }

    /// Parse a complete NUL-terminated control registration.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let terminator = bytes
            .iter()
            .position(|byte| *byte == 0 || *byte == b';')
            .ok_or_else(|| Error::MalformedRegistration("registration is not terminated".into()))?;
        let text =
            String::from_utf8(bytes[..terminator].to_vec()).map_err(|source| Error::Utf8 {
                phase: Phase::Registration,
                source,
            })?;

        let mut name = "Old One-Eyed Dogface".to_owned();
        let mut channel = None;
        let mut acknowledgements = true;
        let mut directives = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if let Some(value) = line.strip_prefix("PONAME ") {
                name = value
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
            } else if let Some(value) = line.strip_prefix("IOCHAN ") {
                channel = Some(Channel::parse(
                    value.split_whitespace().next().unwrap_or_default(),
                )?);
            } else if line == "NO_ACK" {
                acknowledgements = false;
                directives.push(line.to_owned());
            } else {
                directives.push(line.to_owned());
            }
        }
        let registration = Self {
            name,
            channel: channel.ok_or_else(|| {
                Error::MalformedRegistration("registration has no IOCHAN directive".into())
            })?,
            acknowledgements,
            directives,
        };
        registration.validate()?;
        Ok(registration)
    }

    /// Encode the control registration, including its trailing NUL.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut text = format!("PONAME {}\n", self.name);
        for directive in &self.directives {
            if directive != "NO_ACK" || !self.acknowledgements {
                text.push_str(directive);
                text.push('\n');
            }
        }
        if !self.acknowledgements && !self.directives.iter().any(|line| line == "NO_ACK") {
            text.push_str("NO_ACK\n");
        }
        text.push_str("IOCHAN ");
        text.push_str(&self.channel.specification());
        let mut bytes = text.into_bytes();
        bytes.push(0);
        Ok(bytes)
    }

    fn validate(&self) -> Result<()> {
        validate_token("PONAME", &self.name)?;
        match &self.channel {
            Channel::Tcp { host, port } => {
                validate_token("TCP host", host)?;
                if *port == 0 {
                    return Err(Error::MalformedRegistration(
                        "TCP data port must be nonzero".into(),
                    ));
                }
            }
            Channel::SharedMemory(specification) => {
                if specification.contains('\0') || specification.contains('\n') {
                    return Err(Error::EmbeddedNul { field: "IOCHAN" });
                }
            }
        }
        for directive in &self.directives {
            if directive.contains('\0') || directive.contains('\n') {
                return Err(Error::EmbeddedNul {
                    field: "registration directive",
                });
            }
        }
        Ok(())
    }
}

fn validate_token(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(Error::MalformedRegistration(format!(
            "{field} must be one non-whitespace token"
        )));
    }
    if value.contains('\0') {
        return Err(Error::EmbeddedNul { field });
    }
    Ok(())
}

/// Bounds and timeouts applied by both sides of the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Deadline for connecting or accepting each channel.
    pub connect_timeout: Duration,
    /// Per-operation socket read/write timeout.
    pub io_timeout: Duration,
    /// Maximum registration payload, excluding the NUL.
    pub max_registration: usize,
    /// Maximum data payload, excluding the NUL.
    pub max_message: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(9),
            io_timeout: Duration::from_secs(9),
            max_registration: DEFAULT_MAX_REGISTRATION,
            max_message: DEFAULT_MAX_MESSAGE,
        }
    }
}

/// Configuration for a legacy plugout TCP client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// AFNI/afniru host.
    pub host: String,
    /// AFNI control port (`AFNI_PLUGOUT_TCP_0`).
    pub control_port: u16,
    /// Port on which AFNI/afniru will accept the data connection.
    pub data_port: u16,
    /// Informational name registered with the receiver.
    pub name: String,
    /// Transport bounds and deadlines.
    pub limits: Limits,
}

impl ClientConfig {
    /// Configure explicit control and data ports.
    pub fn new(host: impl Into<String>, control_port: u16, data_port: u16) -> Self {
        Self {
            host: host.into(),
            control_port,
            data_port,
            name: "afni-drive".into(),
            limits: Limits::default(),
        }
    }

    /// Configure the ports used by AFNI's official `plugout_drive` program.
    pub fn from_ports(host: impl Into<String>, ports: &Ports) -> Self {
        Self::new(
            host,
            ports
                .get(AFNI_PLUGOUT_TCP_0)
                .expect("AFNI control port is in Ports"),
            ports
                .get(PLUGOUT_DRIVE_PORT)
                .expect("plugout drive port is in Ports"),
        )
    }
}

/// Connected client-side plugout data session.
#[derive(Debug)]
pub struct Client {
    stream: TcpStream,
    reader: NulReader,
    max_message: usize,
}

impl Client {
    /// Perform the control registration and connect the requested data channel.
    pub fn connect(config: &ClientConfig) -> Result<Self> {
        let limits = config.limits;
        let mut control = connect_until(
            &config.host,
            config.control_port,
            limits.connect_timeout,
            Phase::ControlConnect,
        )?;
        configure_stream(&control, limits.io_timeout, Phase::ControlConnect)?;
        let registration =
            Registration::tcp(config.name.clone(), config.host.clone(), config.data_port)?;
        let bytes = registration.encode()?;
        if bytes.len().saturating_sub(1) > limits.max_registration {
            return Err(Error::FrameTooLarge {
                phase: Phase::Registration,
                limit: limits.max_registration,
            });
        }
        write_all(&mut control, &bytes, Phase::Registration)?;
        let acknowledgement = read_ack(&mut control, Phase::RegistrationAck)?;
        if acknowledgement == Acknowledgement::Bad {
            return Err(Error::InvalidState(
                "receiver rejected plugout registration",
            ));
        }
        let _ = control.shutdown(Shutdown::Both);

        let stream = connect_until(
            &config.host,
            config.data_port,
            limits.connect_timeout,
            Phase::DataConnect,
        )?;
        configure_stream(&stream, limits.io_timeout, Phase::DataConnect)?;
        Ok(Self {
            stream,
            reader: NulReader::new(limits.max_message),
            max_message: limits.max_message,
        })
    }

    /// Send one already-prefixed plugout message and wait for its ACK.
    pub fn send_message(&mut self, message: &str) -> Result<Acknowledgement> {
        self.send_message_without_ack(message)?;
        read_ack(&mut self.stream, Phase::DataAck)
    }

    /// Send one `DRIVE_AFNI` command and wait for its ACK.
    pub fn send_drive_afni(&mut self, command: &str) -> Result<Acknowledgement> {
        if command.len() >= DRIVE_AFNI_COMMAND_LIMIT {
            return Err(Error::FrameTooLarge {
                phase: Phase::Data,
                limit: DRIVE_AFNI_COMMAND_LIMIT - 1,
            });
        }
        self.send_message(&format!("{DRIVE_AFNI_PREFIX}{command}"))
    }

    /// Send a message without reading an ACK, as legacy `QUIT` handling does.
    pub fn send_message_without_ack(&mut self, message: &str) -> Result<()> {
        if message.contains('\0') {
            return Err(Error::EmbeddedNul { field: "message" });
        }
        if message.len() > self.max_message {
            return Err(Error::FrameTooLarge {
                phase: Phase::Data,
                limit: self.max_message,
            });
        }
        let mut bytes = Vec::with_capacity(message.len() + 1);
        bytes.extend_from_slice(message.as_bytes());
        bytes.push(0);
        write_all(&mut self.stream, &bytes, Phase::Data)
    }

    /// Receive a NUL-terminated message sent by the receiver.
    pub fn receive_message(&mut self) -> Result<Option<String>> {
        let Some(bytes) = self.reader.read(&mut self.stream, Phase::Data)? else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|source| Error::Utf8 {
                phase: Phase::Data,
                source,
            })
    }

    /// Acknowledge a receiver-originated message.
    pub fn acknowledge(&mut self, acknowledgement: Acknowledgement) -> Result<()> {
        write_all(&mut self.stream, acknowledgement.as_bytes(), Phase::DataAck)
    }
}

/// A listening receiver for official or Rust plugout clients.
#[derive(Debug)]
pub struct Server {
    control: TcpListener,
    data_bind_ip: IpAddr,
    limits: Limits,
}

impl Server {
    /// Bind a control listener. Data listeners use the same local IP.
    pub fn bind(address: impl ToSocketAddrs, limits: Limits) -> Result<Self> {
        let control =
            TcpListener::bind(address).map_err(|source| io_error(Phase::ControlConnect, source))?;
        control
            .set_nonblocking(true)
            .map_err(|source| io_error(Phase::ControlConnect, source))?;
        let data_bind_ip = control
            .local_addr()
            .map_err(|source| io_error(Phase::ControlConnect, source))?
            .ip();
        Ok(Self {
            control,
            data_bind_ip,
            limits,
        })
    }

    /// Address of the control listener.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.control
            .local_addr()
            .map_err(|source| io_error(Phase::ControlConnect, source))
    }

    /// Accept, register, and connect one plugout client.
    pub fn accept(&self) -> Result<ServerSession> {
        let (mut control, _) = accept_until(
            &self.control,
            self.limits.connect_timeout,
            Phase::ControlConnect,
        )?;
        configure_stream(&control, self.limits.io_timeout, Phase::ControlConnect)?;
        let mut registration_reader = NulReader::new(self.limits.max_registration);
        let registration_bytes =
            match registration_reader.read_registration(&mut control, Phase::Registration) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    let _ = write_all(&mut control, &ACK_BAD, Phase::RegistrationAck);
                    return Err(Error::PrematureClose {
                        phase: Phase::Registration,
                        received: 0,
                    });
                }
                Err(error) => {
                    let _ = write_all(&mut control, &ACK_BAD, Phase::RegistrationAck);
                    return Err(error);
                }
            };
        let mut terminated = registration_bytes;
        terminated.push(0);
        let registration = match Registration::parse(&terminated) {
            Ok(registration) => registration,
            Err(error) => {
                let _ = write_all(&mut control, &ACK_BAD, Phase::RegistrationAck);
                return Err(error);
            }
        };
        let port = match &registration.channel {
            Channel::Tcp { port, .. } => *port,
            Channel::SharedMemory(specification) => {
                let _ = write_all(&mut control, &ACK_BAD, Phase::RegistrationAck);
                return Err(Error::UnsupportedChannel(specification.clone()));
            }
        };
        let data_listener = match TcpListener::bind((self.data_bind_ip, port)) {
            Ok(listener) => listener,
            Err(source) => {
                let _ = write_all(&mut control, &ACK_BAD, Phase::RegistrationAck);
                return Err(io_error(Phase::DataConnect, source));
            }
        };
        data_listener
            .set_nonblocking(true)
            .map_err(|source| io_error(Phase::DataConnect, source))?;
        write_all(&mut control, &ACK_OK, Phase::RegistrationAck)?;
        let _ = control.shutdown(Shutdown::Both);

        let (stream, peer_addr) = accept_until(
            &data_listener,
            self.limits.connect_timeout,
            Phase::DataConnect,
        )?;
        configure_stream(&stream, self.limits.io_timeout, Phase::DataConnect)?;
        Ok(ServerSession {
            registration,
            peer_addr,
            stream,
            reader: NulReader::new(self.limits.max_message),
            awaiting_ack: false,
        })
    }
}

/// One message received by a plugout server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    text: String,
}

impl Message {
    /// Complete wire text, excluding its trailing NUL.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Return the command text when this is a `DRIVE_AFNI` message.
    pub fn drive_afni(&self) -> Option<&str> {
        self.text.strip_prefix(DRIVE_AFNI_PREFIX)
    }
}

/// Connected server-side plugout data session.
#[derive(Debug)]
pub struct ServerSession {
    registration: Registration,
    peer_addr: SocketAddr,
    stream: TcpStream,
    reader: NulReader,
    awaiting_ack: bool,
}

impl ServerSession {
    /// Registration supplied on the control channel.
    pub fn registration(&self) -> &Registration {
        &self.registration
    }

    /// Peer address of the established data channel.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Receive the next complete data message.
    pub fn receive(&mut self) -> Result<Option<Message>> {
        if self.awaiting_ack {
            return Err(Error::InvalidState(
                "acknowledge the current message before receiving another",
            ));
        }
        let Some(bytes) = self.reader.read(&mut self.stream, Phase::Data)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|source| Error::Utf8 {
            phase: Phase::Data,
            source,
        })?;
        self.awaiting_ack = self.registration.acknowledgements;
        Ok(Some(Message { text }))
    }

    /// Acknowledge the most recently received message.
    pub fn acknowledge(&mut self, acknowledgement: Acknowledgement) -> Result<()> {
        if !self.registration.acknowledgements {
            return Err(Error::InvalidState(
                "this registration disabled acknowledgements",
            ));
        }
        if !self.awaiting_ack {
            return Err(Error::InvalidState("there is no message awaiting an ACK"));
        }
        write_all(&mut self.stream, acknowledgement.as_bytes(), Phase::DataAck)?;
        self.awaiting_ack = false;
        Ok(())
    }
}

#[derive(Debug)]
struct NulReader {
    buffer: Vec<u8>,
    limit: usize,
}

impl NulReader {
    fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
        }
    }

    fn read(&mut self, stream: &mut TcpStream, phase: Phase) -> Result<Option<Vec<u8>>> {
        self.read_delimited(stream, phase, false)
    }

    fn read_registration(
        &mut self,
        stream: &mut TcpStream,
        phase: Phase,
    ) -> Result<Option<Vec<u8>>> {
        // AFNI's control receiver treats either NUL or semicolon as the end of
        // registration. Data messages remain strictly NUL-terminated.
        self.read_delimited(stream, phase, true)
    }

    fn read_delimited(
        &mut self,
        stream: &mut TcpStream,
        phase: Phase,
        semicolon: bool,
    ) -> Result<Option<Vec<u8>>> {
        let is_terminator = |byte: &u8| *byte == 0 || (semicolon && *byte == b';');
        loop {
            if let Some(end) = self.buffer.iter().position(is_terminator) {
                let mut remainder = self.buffer.split_off(end + 1);
                std::mem::swap(&mut remainder, &mut self.buffer);
                remainder.truncate(end);
                return Ok(Some(remainder));
            }
            if self.buffer.len() > self.limit {
                self.buffer.clear();
                return Err(Error::FrameTooLarge {
                    phase,
                    limit: self.limit,
                });
            }
            let mut chunk = [0_u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) if self.buffer.is_empty() => return Ok(None),
                Ok(0) => {
                    let received = self.buffer.len();
                    self.buffer.clear();
                    return Err(Error::PrematureClose { phase, received });
                }
                Ok(count) => {
                    self.buffer.extend_from_slice(&chunk[..count]);
                    if self.buffer.len() > self.limit + 1
                        && !self.buffer[..=self.limit].iter().any(is_terminator)
                    {
                        self.buffer.clear();
                        return Err(Error::FrameTooLarge {
                            phase,
                            limit: self.limit,
                        });
                    }
                }
                Err(source) => return Err(io_error(phase, source)),
            }
        }
    }
}

fn configure_stream(stream: &TcpStream, timeout: Duration, phase: Phase) -> Result<()> {
    // Listeners are nonblocking so accepts can have a deadline. On platforms
    // where accepted sockets inherit that flag, restore ordinary blocking I/O
    // before applying read/write timeouts.
    stream
        .set_nonblocking(false)
        .map_err(|source| io_error(phase, source))?;
    stream
        .set_nodelay(true)
        .map_err(|source| io_error(phase, source))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|source| io_error(phase, source))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|source| io_error(phase, source))?;
    Ok(())
}

fn write_all(stream: &mut TcpStream, bytes: &[u8], phase: Phase) -> Result<()> {
    stream
        .write_all(bytes)
        .and_then(|()| stream.flush())
        .map_err(|source| io_error(phase, source))
}

fn read_ack(stream: &mut TcpStream, phase: Phase) -> Result<Acknowledgement> {
    let mut bytes = [0_u8; ACK_SIZE];
    stream
        .read_exact(&mut bytes)
        .map_err(|source| io_error(phase, source))?;
    Acknowledgement::from_bytes(bytes)
}

fn connect_until(host: &str, port: u16, timeout: Duration, phase: Phase) -> Result<TcpStream> {
    let label = format!("{host}:{port}");
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|source| io_error(phase, source))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(Error::NoAddresses(label));
    }
    let started = Instant::now();
    let per_attempt = Duration::from_millis(250).min(timeout.max(Duration::from_millis(1)));
    let mut last_error = None;
    loop {
        for address in &addresses {
            match TcpStream::connect_timeout(address, per_attempt) {
                Ok(stream) => return Ok(stream),
                Err(source) => last_error = Some(source),
            }
        }
        if started.elapsed() >= timeout {
            return match last_error {
                Some(source) if source.kind() != ErrorKind::TimedOut => {
                    Err(io_error(phase, source))
                }
                _ => Err(Error::Timeout { phase, timeout }),
            };
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn accept_until(
    listener: &TcpListener,
    timeout: Duration,
    phase: Phase,
) -> Result<(TcpStream, SocketAddr)> {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok(connection) => return Ok(connection),
            Err(source) if source.kind() == ErrorKind::WouldBlock => {
                if started.elapsed() >= timeout {
                    return Err(Error::Timeout { phase, timeout });
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(source) => return Err(io_error(phase, source)),
        }
    }
}

fn io_error(phase: Phase, source: std::io::Error) -> Error {
    Error::Io { phase, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_round_trip_and_semicolon_compatibility() {
        let mut registration = Registration::tcp("afni-drive", "localhost", 8009).unwrap();
        registration.directives.push("DICOM_XYZ_DELTA".into());
        let encoded = registration.encode().unwrap();
        assert_eq!(Registration::parse(&encoded).unwrap(), registration);

        let parsed = Registration::parse(b"PONAME p\nIOCHAN tcp:host:9000;ignored").unwrap();
        assert_eq!(parsed.name, "p");
        assert_eq!(
            parsed.channel,
            Channel::Tcp {
                host: "host".into(),
                port: 9000
            }
        );
    }

    #[test]
    fn acknowledgements_are_exact() {
        assert_eq!(
            Acknowledgement::from_bytes(ACK_OK).unwrap(),
            Acknowledgement::Ok
        );
        assert_eq!(
            Acknowledgement::from_bytes(ACK_BAD).unwrap(),
            Acknowledgement::Bad
        );
        assert!(Acknowledgement::from_bytes(*b"OK!!").is_err());
    }
}
