//! Target-neutral commands and NIML framing for the DriveSuma protocol.
//!
//! It owns the target-neutral command model, exact wire representation, and
//! reusable TCP session. User-interface prompts and process/CLI policy remain
//! with callers such as `drivesuma-rs`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::array::TypedArray;
use crate::niml::{NimlData, NimlElement, NumericMatrix};
use crate::surface::Surface;
use crate::Result;

/// Legacy `-com` grammar and its ordered action model.
pub mod command;

const CLOSE_PROCESSING_INSTRUCTION: &[u8] = b"<?ni_do ni_verb='close_this' ?>\n";

/// A duration accepted by DriveSuma (`2`, `2s`, or `150ms`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DurationArg(pub Duration);

impl DurationArg {
    /// Parse a legacy DriveSuma duration.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        let (number, multiplier) = if let Some(value) = value.strip_suffix("ms") {
            (value, 0.001)
        } else if let Some(value) = value.strip_suffix('s') {
            (value, 1.0)
        } else {
            (value, 1.0)
        };
        let seconds = number
            .parse::<f64>()
            .map_err(|_| crate::Error::invalid(format!("invalid duration {value:?}")))?
            * multiplier;
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(crate::Error::invalid(format!("invalid duration {value:?}")));
        }
        Ok(Self(Duration::from_secs_f64(seconds)))
    }
}

impl fmt::Display for DurationArg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}s", self.0.as_secs_f64())
    }
}

/// A target-neutral SUMA `EngineCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommand {
    /// Value of the wire-level `Command` attribute.
    pub name: String,
    /// Command attributes, excluding `Command` and `Tracking_ID`.
    pub attributes: BTreeMap<String, String>,
}

impl RemoteCommand {
    /// Convert this command into its wire-level NIML element.
    pub fn to_element(&self, tracking_id: usize, working_dir: Option<&Path>) -> NimlElement {
        let mut attributes = self.attributes.clone();
        if !attributes.contains_key("Caller_Working_Dir") && self.name != "kill_suma" {
            if let Some(working_dir) = working_dir {
                attributes.insert(
                    "Caller_Working_Dir".into(),
                    working_dir.to_string_lossy().into_owned(),
                );
            }
        }
        attributes.insert("Command".into(), self.name.clone());
        attributes.insert("Tracking_ID".into(), tracking_id.to_string());
        NimlElement::group("EngineCommand", attributes, Vec::new())
    }
}

/// A complete surface transfer or coordinate update.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceCommand {
    /// Introduce a new triangulated surface to the target.
    Show {
        /// Label used by later controller commands.
        label: String,
        /// Surface state/layer name.
        state: String,
        /// `1` for outward/CCW normals, `-1` for inward/CW normals.
        normal_direction: i32,
        /// Name of the software/file format that created the mesh.
        software: String,
        /// Anatomical side (`left`, `right`, or `unknown`).
        side: String,
        /// Geometry to send.
        surface: Surface,
    },
    /// Replace the coordinates of an existing surface.
    NodeXyz {
        /// Label of the target surface.
        label: String,
        /// Replacement node coordinates.
        vertices: Vec<[f32; 3]>,
    },
}

/// A connected DriveSuma TCP stream.
#[derive(Debug)]
pub struct Session {
    stream: TcpStream,
    stream_name: String,
    bytes_written: usize,
}

impl Session {
    /// Connect with DriveSuma-style retries and configure stream timeouts.
    pub fn open(
        host: &str,
        port: u16,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Result<Self> {
        let stream = connect(host, port, connect_timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(io_timeout))?;
        stream.set_write_timeout(Some(io_timeout))?;
        let peer = stream.peer_addr()?;
        Ok(Self {
            stream,
            stream_name: format!("tcp:{}:{}", peer.ip(), peer.port()),
            bytes_written: 0,
        })
    }

    /// NIML stream name corresponding to the connected peer.
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }

    /// Write and flush one protocol fragment.
    pub fn write(&mut self, payload: &[u8]) -> Result<()> {
        self.stream.write_all(payload)?;
        self.stream.flush()?;
        self.bytes_written += payload.len();
        Ok(())
    }

    /// Close the write side and collect any response until EOF or timeout.
    pub fn close(mut self) -> Result<(usize, Vec<u8>)> {
        self.stream.shutdown(Shutdown::Write)?;
        let mut response = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => response.extend_from_slice(&chunk[..count]),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    break;
                }
                Err(error) if error.kind() == ErrorKind::ConnectionReset => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok((self.bytes_written, response))
    }
}

/// Connect, build one complete stream using its resolved stream name, send it,
/// and wait for the receiver to close.
pub fn send(
    host: &str,
    port: u16,
    connect_timeout: Duration,
    close_timeout: Duration,
    payload_for_stream: impl FnOnce(&str) -> Vec<u8>,
) -> Result<usize> {
    let mut session = Session::open(host, port, connect_timeout, close_timeout)?;
    let payload = payload_for_stream(session.stream_name());
    session.write(&payload)?;
    let (written, _) = session.close()?;
    Ok(written)
}

/// Encode the `StartTracking` element that opens a DriveSuma stream.
pub fn start(stream_name: &str) -> Vec<u8> {
    crate::niml::serialize(&[empty_element(
        "StartTracking",
        BTreeMap::from([
            ("Tracking_ID".into(), "1".into()),
            ("ni_stream_name".into(), stream_name.into()),
        ]),
    )])
    .into_bytes()
}

/// Encode one `EngineCommand`.
pub fn encode_command(command: &RemoteCommand, tracking_id: usize, working_dir: &Path) -> Vec<u8> {
    crate::niml::serialize(&[command.to_element(tracking_id, Some(working_dir))]).into_bytes()
}

/// Encode one binary surface command and return its tracking-ID consumption.
pub fn encode_surface(command: &SurfaceCommand, tracking_id: usize) -> Result<(Vec<u8>, usize)> {
    match command {
        SurfaceCommand::Show {
            label,
            state,
            normal_direction,
            software,
            side,
            surface,
        } => {
            let surface_id = stable_id(label);
            let node_id = stable_id(&format!("{label}:nodes"));
            let mesh_id = stable_id(&format!("{label}:mesh"));
            let attrs = BTreeMap::from([
                ("Object_Type".into(), "Triangulated_Surface".into()),
                ("self_idcode".into(), surface_id.clone()),
                ("Object_Label".into(), label.clone()),
                ("domain_parent_idcode".into(), "~".into()),
                ("Grand_domain_parent_idcode".into(), "~".into()),
                ("Subject_ID".into(), "~".into()),
                ("Subject_Label".into(), "DefGroup".into()),
                ("Instance_ID".into(), "~".into()),
                ("Instance_Label".into(), "~".into()),
                ("do_type".into(), "1".into()),
                ("Side".into(), side.clone()),
                ("Layer_Name".into(), state.clone()),
                ("Anatomically_Correct".into(), "yes".into()),
                ("Embedding_Dimension".into(), "3".into()),
                ("Surface_Creation_Software".into(), software.clone()),
                ("Surface_Creation_History".into(), "~".into()),
                ("Node_Normal_Direction".into(), normal_direction.to_string()),
                ("Mesh_Element_ID".into(), mesh_id.clone()),
                ("NodeList_Element_ID".into(), node_id.clone()),
                ("Tracking_ID".into(), tracking_id.to_string()),
            ]);
            let node_attrs = BTreeMap::from([
                ("self_idcode".into(), node_id),
                ("domain_parent_idcode".into(), surface_id.clone()),
                ("geometry_parent_idcode".into(), "~".into()),
                ("Node_Dim".into(), "3".into()),
                (
                    "Object_Label".into(),
                    format!("NodeList for surface {label}"),
                ),
                ("Target_Object_Label".into(), label.clone()),
            ]);
            let nodes = surface.vertices.iter().flatten().copied().collect();
            let nodes = NimlElement::numeric(
                "NewNode_XYZ",
                node_attrs,
                NumericMatrix::from_columns(vec![TypedArray::Float32(nodes)])?,
            );
            let mesh_attrs = BTreeMap::from([
                ("self_idcode".into(), mesh_id),
                ("domain_parent_idcode".into(), surface_id.clone()),
                ("geometry_parent_idcode".into(), "~".into()),
                ("Mesh_Dim".into(), "3".into()),
                (
                    "Object_Label".into(),
                    format!("FaceSetList for surface {label}"),
                ),
            ]);
            let triangles = surface
                .faces
                .iter()
                .flatten()
                .map(|value| *value as i32)
                .collect();
            let mesh = NimlElement::numeric(
                "NewMesh_IJK",
                mesh_attrs,
                NumericMatrix::from_columns(vec![TypedArray::Int32(triangles)])?,
            );
            let object = NimlElement::group("SurfaceObject", attrs, vec![nodes, mesh]);
            let prep = empty_element(
                "PrepNewSurface",
                BTreeMap::from([
                    ("Tracking_ID".into(), (tracking_id + 1).to_string()),
                    ("surface_idcode".into(), surface_id),
                ]),
            );
            Ok((crate::niml::serialize_binary(&[object, prep]), 2))
        }
        SurfaceCommand::NodeXyz { label, vertices } => {
            let surface_id = stable_id(label);
            let attrs = BTreeMap::from([
                ("self_idcode".into(), stable_id(&format!("{label}:update"))),
                ("domain_parent_idcode".into(), surface_id),
                ("geometry_parent_idcode".into(), "~".into()),
                ("Node_Dim".into(), "3".into()),
                (
                    "Object_Label".into(),
                    format!("NodeList for surface {label}"),
                ),
                ("Target_Object_Label".into(), label.clone()),
                ("Tracking_ID".into(), tracking_id.to_string()),
            ]);
            let values = vertices.iter().flatten().copied().collect();
            let element = NimlElement::numeric(
                "Node_XYZ",
                attrs,
                NumericMatrix::from_columns(vec![TypedArray::Float32(values)])?,
            );
            Ok((crate::niml::serialize_binary(&[element]), 1))
        }
    }
}

/// Encode the stream tail, omitting target-closing commands after `kill_suma`.
pub fn finish(stream_name: &str, killed: bool) -> Vec<u8> {
    let mut elements = Vec::new();
    if !killed {
        elements.push(empty_element(
            "StopTracking",
            BTreeMap::from([("ni_stream_name".into(), stream_name.into())]),
        ));
        elements.push(empty_element(
            "CloseKillStream",
            BTreeMap::from([("ni_stream_name".into(), stream_name.into())]),
        ));
    }
    let mut bytes = crate::niml::serialize(&elements).into_bytes();
    bytes.extend_from_slice(CLOSE_PROCESSING_INSTRUCTION);
    bytes
}

/// Build a complete stream containing only remote engine commands.
pub fn encode(commands: &[RemoteCommand], stream_name: &str, working_dir: &Path) -> Vec<u8> {
    let mut bytes = start(stream_name);
    for (index, command) in commands.iter().enumerate() {
        bytes.extend(encode_command(command, index + 2, working_dir));
    }
    let killed = commands.iter().any(|command| command.name == "kill_suma");
    bytes.extend(finish(stream_name, killed));
    bytes
}

/// Produce a stable NIML idcode from a human-readable label.
pub fn stable_id(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("RS_{hash:016x}")
}

fn empty_element(name: &str, attrs: BTreeMap<String, String>) -> NimlElement {
    NimlElement {
        name: name.into(),
        attrs,
        data: NimlData::None,
    }
}

fn connect(host: &str, port: u16, max_wait: Duration) -> Result<TcpStream> {
    let addresses = (host, port).to_socket_addrs()?.collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(crate::Error::invalid(format!(
            "{host}:{port} resolved to no addresses"
        )));
    }
    let started = Instant::now();
    let attempt_timeout = Duration::from_millis(500).min(max_wait.max(Duration::from_millis(1)));
    let mut last_error = None;
    loop {
        for address in &addresses {
            match TcpStream::connect_timeout(address, attempt_timeout) {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        if started.elapsed() >= max_wait {
            return Err(last_error
                .unwrap_or_else(|| std::io::Error::new(ErrorKind::TimedOut, "connection timed out"))
                .into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::niml::parse_bytes;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn command_stream_has_tracking_and_absolute_working_directory() {
        let command = RemoteCommand {
            name: "surf_cont".into(),
            attributes: BTreeMap::from([("SO_label".into(), "lh.demo".into())]),
        };
        let bytes = encode(&[command], "tcp:127.0.0.1:25032", Path::new("/demo"));
        let elements = parse_bytes(&bytes).unwrap();
        assert_eq!(elements[0].name, "StartTracking");
        assert_eq!(elements[1].attrs["Command"], "surf_cont");
        assert_eq!(elements[1].attrs["Caller_Working_Dir"], "/demo");
        assert_eq!(elements[2].name, "StopTracking");
        assert!(bytes.ends_with(CLOSE_PROCESSING_INSTRUCTION));
    }

    #[test]
    fn surface_payload_has_legacy_shape() {
        let command = SurfaceCommand::Show {
            label: "lh.toy".into(),
            state: "toy".into(),
            normal_direction: 1,
            software: "GIFTI".into(),
            side: "left".into(),
            surface: Surface::from_geometry(
                vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                vec![[0, 1, 2]],
            ),
        };
        let (bytes, consumed) = encode_surface(&command, 2).unwrap();
        let elements = parse_bytes(&bytes).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(elements[0].name, "SurfaceObject");
        assert_eq!(elements[1].attrs["Tracking_ID"], "3");
    }

    #[test]
    fn session_sends_a_complete_stream_over_loopback() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let receiver = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).unwrap();
            tx.send(bytes).unwrap();
        });
        let sent = send(
            "127.0.0.1",
            port,
            Duration::from_secs(1),
            Duration::from_secs(1),
            |stream_name| {
                let mut bytes = start(stream_name);
                bytes.extend(finish(stream_name, false));
                bytes
            },
        )
        .unwrap();
        let bytes = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        receiver.join().unwrap();
        assert_eq!(sent, bytes.len());
        assert_eq!(parse_bytes(&bytes).unwrap()[0].name, "StartTracking");
    }
}
