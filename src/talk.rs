//! Encoding for the AFNI ⇄ SUMA "talk" protocol (feature `talk`).
//!
//! AFNI, SUMA and DriveSuma exchange NIML elements over TCP. This module has
//! everything about that exchange **except the sockets**: which port a program
//! listens on, how a message is framed on the wire, and how the surface,
//! crosshair and colour elements are built and read. Opening, reading and
//! writing sockets (and deciding what to do with a message) stays with the
//! caller.
//!
//! * [`Ports`] reproduces AFNI's port table (`afni_ports.c`): the `-np` /
//!   `-npb` offsets, the `AFNI_PORT_OFFSET` / `AFNI_PORT_BLOC` environment
//!   variables, and the legacy fixed ports.
//! * [`encode`], [`registration_bytes`], [`MessageReader`] and the
//!   [`KEEP_READING`] / [`PAUSE_READING`] processing instructions are the wire
//!   framing. Everything is sent as binary NIML, as AFNI and SUMA do.
//! * [`SurfaceInfo`], [`Geometry`] and [`surface_elements`] build the
//!   `SUMA_ixyz`, `SUMA_node_normals` and `SUMA_ijk` elements that register a
//!   surface with AFNI. [`Crosshair`] and [`Irgba`] build and read the
//!   messages that flow afterwards.
//! * [`drive_afni_element`] and [`switch_underlay_command`] build `DRIVE_AFNI`
//!   commands.
//!
//! Coordinates in every message are AFNI's native RAI (DICOM) order. A GIfTI
//! surface is in RAS, so flip x and y first with [`flip_xy`].
//!
//! ```
//! use afni_io::talk::{Ports, AFNI_SUMA_NIML};
//!
//! // `afni -niml -npb 3` and `suma -niml -npb 3` meet on this port.
//! let ports = Ports::new(Some(afni_io::talk::offset_from_bloc(3)?))?;
//! assert_eq!(ports.get(AFNI_SUMA_NIML), Some(1096));
//! # Ok::<(), afni_io::Error>(())
//! ```

use std::collections::BTreeMap;

use crate::array::TypedArray;
use crate::niml::{self, NimlData, NimlElement, NumericMatrix};
use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// Every named port, in AFNI's order. A port's index is its offset from the
/// base port when a `-np` offset is given.
pub const PORT_NAMES: [&str; 24] = [
    "AFNI_SUMA_NIML",
    "AFNI_DEFAULT_LISTEN_NIML",
    "AFNI_GroupInCorr_NIML",
    "SUMA_DEFAULT_LISTEN_NIML",
    "SUMA_GroupInCorr_NIML",
    "MATLAB_SUMA_NIML",
    "SUMA_GEOMCOMP_NIML",
    "SUMA_BRAINWRAP_NIML",
    "SUMA_DRIVESUMA_NIML",
    "AFNI_PLUGOUT_TCP_0",
    "AFNI_PLUGOUT_TCP_1",
    "AFNI_PLUGOUT_TCP_2",
    "AFNI_PLUGOUT_TCP_3",
    "AFNI_PLUGOUT_TCP_4",
    "AFNI_TCP_PORT",
    "AFNI_CONTROL_PORT",
    "PLUGOUT_DRIVE_PORT",
    "PLUGOUT_GRAPH_PORT",
    "PLUGOUT_IJK_PORT",
    "PLUGOUT_SURF_PORT",
    "PLUGOUT_TT_PORT",
    "PLUGOUT_TTA_PORT",
    "SUMA_HALLO_SUMA_NIML",
    "SUMA_INSTA_TRACT_NIML",
];

/// The port AFNI listens on for SUMA.
pub const AFNI_SUMA_NIML: &str = "AFNI_SUMA_NIML";
/// The port SUMA listens on for DriveSuma.
pub const SUMA_DRIVESUMA_NIML: &str = "SUMA_DRIVESUMA_NIML";

/// The base port used when no offset is given.
pub const DEFAULT_PORT_OFFSET: u16 = 53211;
/// Smallest valid `-np` offset (it is also where block 0 starts).
pub const MIN_PORT_OFFSET: u16 = 1024;
/// Largest valid `-np` offset.
pub const MAX_PORT_OFFSET: u16 = 65500;
/// Largest valid `-npb` block: `(65535 - 1024) / 24 - 1`.
pub const MAX_PORT_BLOC: u16 = 2686;

const PLUGOUT_BASE_DEFAULT: u32 = 7955;
const LEGACY_FIXED_PORTS: [u32; 6] = [8099, 8077, 8009, 8019, 8001, 8005];

/// Check a `-np` offset (1024 to 65500, as AFNI does).
pub fn check_offset(offset: u16) -> Result<u16> {
    if (MIN_PORT_OFFSET..=MAX_PORT_OFFSET).contains(&offset) {
        Ok(offset)
    } else {
        Err(Error::invalid(format!(
            "AFNI port offset {offset} is outside {MIN_PORT_OFFSET}..={MAX_PORT_OFFSET}"
        )))
    }
}

/// The `-np` offset of an `-npb` block: `1024 + bloc * 24`.
pub fn offset_from_bloc(bloc: u16) -> Result<u16> {
    if bloc > MAX_PORT_BLOC {
        return Err(Error::invalid(format!(
            "AFNI port bloc {bloc} is outside 0..={MAX_PORT_BLOC}"
        )));
    }
    Ok(MIN_PORT_OFFSET + bloc * PORT_NAMES.len() as u16)
}

/// The block an offset falls in (`(offset - 1024) / 24`), or `None` below 1024.
pub fn bloc_from_offset(offset: u16) -> Option<u16> {
    (offset >= MIN_PORT_OFFSET).then(|| (offset - MIN_PORT_OFFSET) / PORT_NAMES.len() as u16)
}

/// The offset set by the environment, as AFNI reads it: `AFNI_PORT_BLOC`
/// (0 or more) wins, then `AFNI_PORT_OFFSET` (1024 or more). A variable that
/// is unset, not a number, or (for the offset) below 1024 is skipped.
///
/// A number that is set but out of range is an error here. AFNI prints a
/// message and carries on with the default ports (and, for a bad bloc, does
/// not try `AFNI_PORT_OFFSET`); a caller that wants that can use
/// `.unwrap_or(None)`.
pub fn offset_from_env(get: impl Fn(&str) -> Option<String>) -> Result<Option<u16>> {
    if let Some(bloc) = env_number(&get, "AFNI_PORT_BLOC").filter(|v| *v >= 0) {
        let bloc = u16::try_from(bloc)
            .ok()
            .filter(|b| *b <= MAX_PORT_BLOC)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "AFNI_PORT_BLOC {bloc} is outside 0..={MAX_PORT_BLOC}"
                ))
            })?;
        return offset_from_bloc(bloc).map(Some);
    }
    if let Some(offset) = env_number(&get, "AFNI_PORT_OFFSET").filter(|v| *v >= 1024) {
        let offset = u16::try_from(offset)
            .map_err(|_| Error::invalid(format!("AFNI_PORT_OFFSET {offset} is too large")))?;
        return check_offset(offset).map(Some);
    }
    Ok(None)
}

fn env_number(get: &impl Fn(&str) -> Option<String>, name: &str) -> Option<i64> {
    get(name)?.trim().parse().ok()
}

/// Environment variables that move individual ports when no offset is given.
/// With an offset they are ignored, except `AFNI_PLUGOUT_TCP_BASE`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortEnv {
    /// `SUMA_AFNI_TCP_PORT`: the `AFNI_SUMA_NIML` port.
    pub suma_afni_tcp_port: Option<u16>,
    /// `SUMA_AFNI_TCP_PORT2`: the `AFNI_DEFAULT_LISTEN_NIML` port.
    pub suma_afni_tcp_port2: Option<u16>,
    /// `SUMA_MATLAB_LISTEN_PORT`: the `MATLAB_SUMA_NIML` port.
    pub suma_matlab_listen_port: Option<u16>,
    /// `AFNI_PLUGOUT_TCP_BASE`: the first of the seven plugout TCP ports.
    pub afni_plugout_tcp_base: Option<u16>,
}

impl PortEnv {
    /// Read the variables through `get`. A value of 0 or one that is not a
    /// port number counts as unset, as it does for AFNI.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let port = |name: &str| {
            env_number(&get, name)
                .and_then(|v| u16::try_from(v).ok())
                .filter(|v| *v > 0)
        };
        Self {
            suma_afni_tcp_port: port("SUMA_AFNI_TCP_PORT"),
            suma_afni_tcp_port2: port("SUMA_AFNI_TCP_PORT2"),
            suma_matlab_listen_port: port("SUMA_MATLAB_LISTEN_PORT"),
            afni_plugout_tcp_base: port("AFNI_PLUGOUT_TCP_BASE"),
        }
    }
}

/// AFNI's port table for one offset (or for none).
///
/// Built the way `init_ports_list` builds it. Port `i` is `offset + i`,
/// except that with **no** offset the plugout ports (indices 9 to 21) keep
/// their legacy numbers (7955 and up, 8099, 8077, 8009, 8019, 8001, 8005).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ports {
    offset: Option<u16>,
    ports: [u16; 24],
}

impl Ports {
    /// The table for `offset` (`None` for AFNI's defaults), without any
    /// per-port environment overrides.
    pub fn new(offset: Option<u16>) -> Result<Self> {
        Self::with_env(offset, &PortEnv::default())
    }

    /// The table for `offset`, applying `env` the way AFNI does.
    pub fn with_env(offset: Option<u16>, env: &PortEnv) -> Result<Self> {
        if let Some(offset) = offset {
            check_offset(offset)?;
        }
        let user = offset.is_some();
        let np = u32::from(offset.unwrap_or(DEFAULT_PORT_OFFSET));
        let at = |i: usize| np + i as u32;
        let over = |value: Option<u16>, i: usize| match value {
            Some(port) if !user => u32::from(port),
            _ => at(i),
        };

        let plugout_base = match env.afni_plugout_tcp_base {
            Some(base) if base >= 1024 => u32::from(base),
            _ if user => at(9),
            _ => PLUGOUT_BASE_DEFAULT,
        };

        let mut wide = [0u32; 24];
        for (i, slot) in wide.iter_mut().enumerate() {
            *slot = match i {
                0 => over(env.suma_afni_tcp_port, 0),
                1 => over(env.suma_afni_tcp_port2, 1),
                5 => over(env.suma_matlab_listen_port, 5),
                9..=15 => plugout_base + (i as u32 - 9),
                16..=21 if !user => LEGACY_FIXED_PORTS[i - 16],
                _ => at(i),
            };
        }
        let mut ports = [0u16; 24];
        for (port, wide) in ports.iter_mut().zip(wide) {
            *port = u16::try_from(wide)
                .map_err(|_| Error::invalid(format!("AFNI port {wide} exceeds 65535")))?;
        }
        Ok(Self { offset, ports })
    }

    /// The offset this table was built for.
    pub fn offset(&self) -> Option<u16> {
        self.offset
    }

    /// The block of the offset, if there is one at or above 1024.
    pub fn bloc(&self) -> Option<u16> {
        self.offset.and_then(bloc_from_offset)
    }

    /// The port named `name` (one of [`PORT_NAMES`]).
    pub fn get(&self, name: &str) -> Option<u16> {
        PORT_NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .map(|i| self.ports[i])
    }

    /// The port AFNI listens on for SUMA.
    pub fn afni_suma_niml(&self) -> u16 {
        self.ports[0]
    }

    /// The port SUMA listens on for DriveSuma.
    pub fn drivesuma(&self) -> u16 {
        self.ports[8]
    }

    /// Every `(name, port)` pair, in AFNI's order (as `afni -list_ports`).
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, u16)> + '_ {
        PORT_NAMES.iter().copied().zip(self.ports.iter().copied())
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Processing instruction telling AFNI to keep reading without pausing. Send
/// it before a burst of large elements (a surface registration) so AFNI does
/// not wait between them. AFNI writes processing instructions as
/// `<?name ?>\n` (`NI_write_procins`).
pub const KEEP_READING: &[u8] = b"<?keep_reading ?>\n";
/// Ends a [`KEEP_READING`] burst.
pub const PAUSE_READING: &[u8] = b"<?pause_reading ?>\n";

/// Encode elements for the wire: binary NIML (`binary.lsbfirst`) for numeric
/// bodies, ASCII for groups and text, which AFNI accepts mixed in one stream.
pub fn encode(elements: &[NimlElement]) -> Vec<u8> {
    niml::serialize_binary(elements)
}

/// The processing-instruction form of a `DRIVE_AFNI` command,
/// `<?drive_afni cmd='...' ?>`. The command cannot contain a single quote.
pub fn drive_afni_procins(command: &str) -> Result<Vec<u8>> {
    if command.contains('\'') || command.contains('\n') {
        return Err(Error::invalid(
            "a drive_afni processing instruction cannot hold a quote or newline",
        ));
    }
    Ok(format!("<?drive_afni cmd='{command}' ?>\n").into_bytes())
}

/// The element form of a `DRIVE_AFNI` command:
/// `<ni_do ni_verb="DRIVE_AFNI" ni_object="..." />`.
pub fn drive_afni_element(command: impl Into<String>) -> NimlElement {
    let attrs = BTreeMap::from([
        ("ni_verb".to_string(), "DRIVE_AFNI".to_string()),
        ("ni_object".to_string(), command.into()),
    ]);
    NimlElement {
        name: "ni_do".to_string(),
        attrs,
        data: NimlData::None,
    }
}

/// `SWITCH_UNDERLAY A.<dataset>`, naming the surface's parent volume by
/// idcode, else head name, else the file name of its file code.
pub fn switch_underlay_command(info: &SurfaceInfo) -> Option<String> {
    let non_empty = |s: &&str| !s.is_empty();
    let target = info
        .volume_idcode
        .as_deref()
        .filter(non_empty)
        .or(info.volume_headname.as_deref().filter(non_empty))
        .or_else(|| {
            info.volume_filecode
                .as_deref()
                .and_then(|path| std::path::Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .filter(non_empty)
        })?;
    Some(format!("SWITCH_UNDERLAY A.{target}"))
}

const DEFAULT_MAX_PENDING: usize = 256 * 1024 * 1024;

/// Turns the bytes read from a socket into complete elements.
///
/// Feed it whatever each `read` returns. An element split across reads is held
/// until the rest arrives; a binary element is recognised as incomplete from
/// its header alone, so a large one arriving in small chunks is not re-scanned
/// each time. Processing instructions are consumed and dropped.
///
/// An element whose closing tag never comes looks the same as one still
/// arriving, so the buffer is capped ([`MessageReader::with_max_pending`]).
/// Any error (malformed input, or the cap) discards the buffer: the stream is
/// out of step and the connection should be closed.
#[derive(Debug, Clone)]
pub struct MessageReader {
    buffer: Vec<u8>,
    max_pending: usize,
}

impl Default for MessageReader {
    fn default() -> Self {
        Self::with_max_pending(DEFAULT_MAX_PENDING)
    }
}

impl MessageReader {
    /// A reader that buffers at most 256 MiB.
    pub fn new() -> Self {
        Self::default()
    }

    /// A reader that buffers at most `max_pending` bytes of an unfinished
    /// element.
    pub fn with_max_pending(max_pending: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_pending,
        }
    }

    /// Add `bytes` and return every element that is now complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<NimlElement>> {
        self.buffer.extend_from_slice(bytes);
        match niml::parse_stream(&self.buffer) {
            Ok((elements, consumed)) => {
                self.buffer.drain(..consumed);
                if self.buffer.len() > self.max_pending {
                    let pending = self.buffer.len();
                    self.buffer = Vec::new();
                    return Err(Error::invalid(format!(
                        "NIML element still incomplete after {pending} bytes \
                         (limit {})",
                        self.max_pending
                    )));
                }
                Ok(elements)
            }
            Err(error) => {
                self.buffer = Vec::new();
                Err(error)
            }
        }
    }

    /// Bytes held for an element that has not finished arriving.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Drop anything held (for example after reconnecting).
    pub fn clear(&mut self) {
        self.buffer = Vec::new();
    }
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Colours and visibility AFNI gives a surface it has just been sent
/// (`afni_surface_controls_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceControls {
    /// Whether the surface is drawn (`on`/`off`).
    pub toggle: bool,
    /// Node marker colour, or `none`.
    pub nodes: String,
    /// Line colour as `#rrggbb`, or `none`.
    pub lines: String,
    /// The `+`/`-` marker colour, or `none`.
    pub plusminus: String,
}

impl SurfaceControls {
    /// Visible, lines only, coloured by what the label suggests: green for a
    /// white-matter/`smoothwm` surface (yellow on the right hemisphere), blue
    /// for a left pial surface and red for other pial surfaces, pink otherwise.
    pub fn for_label(label: &str) -> Self {
        let label = label.to_ascii_lowercase();
        let left = label.contains("lh") || label.contains("left");
        let right = label.contains("rh") || label.contains("right");
        let lines = if label.contains("smoothwm") || label.contains("white") {
            if right {
                "#ffff00"
            } else {
                "#00ff00"
            }
        } else if label.contains("pial") {
            if left {
                "#0000ff"
            } else {
                "#ff0000"
            }
        } else {
            "#ff69b4"
        };
        Self {
            toggle: true,
            nodes: "none".to_string(),
            lines: lines.to_string(),
            plusminus: "none".to_string(),
        }
    }
}

/// What AFNI is told about a surface, copied onto every registration element
/// (`SUMA_surface_info` in `SUMA_niml.c`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceInfo {
    /// `surface_idcode`. AFNI requires it.
    pub surface_idcode: String,
    /// `surface_label`.
    pub surface_label: String,
    /// `local_domain_parent_ID`.
    pub local_domain_parent_id: String,
    /// `local_domain_parent`.
    pub local_domain_parent: String,
    /// `surface_specfile_name`.
    pub specfile_name: Option<String>,
    /// `surface_specfile_path`.
    pub specfile_path: Option<String>,
    /// `volume_idcode`: the surface's parent volume.
    pub volume_idcode: Option<String>,
    /// `volume_headname`.
    pub volume_headname: Option<String>,
    /// `volume_filecode`.
    pub volume_filecode: Option<String>,
    /// `volume_dirname`.
    pub volume_dirname: Option<String>,
    /// `afni_surface_controls_*`, if AFNI should be told how to draw it.
    pub controls: Option<SurfaceControls>,
}

impl SurfaceInfo {
    /// The attributes shared by `SUMA_ixyz`, `SUMA_node_normals` and `SUMA_ijk`.
    pub fn attributes(&self) -> BTreeMap<String, String> {
        let mut attrs = BTreeMap::new();
        let mut put = |key: &str, value: &str| {
            attrs.insert(key.to_string(), value.to_string());
        };
        put("surface_idcode", &self.surface_idcode);
        put("surface_label", &self.surface_label);
        put("local_domain_parent_ID", &self.local_domain_parent_id);
        put("local_domain_parent", &self.local_domain_parent);
        for (key, value) in [
            ("surface_specfile_name", &self.specfile_name),
            ("surface_specfile_path", &self.specfile_path),
            ("volume_idcode", &self.volume_idcode),
            ("volume_headname", &self.volume_headname),
            ("volume_filecode", &self.volume_filecode),
            ("volume_dirname", &self.volume_dirname),
        ] {
            if let Some(value) = value {
                put(key, value);
            }
        }
        if let Some(controls) = &self.controls {
            put(
                "afni_surface_controls_toggle",
                if controls.toggle { "on" } else { "off" },
            );
            put("afni_surface_controls_nodes", &controls.nodes);
            put("afni_surface_controls_lines", &controls.lines);
            put("afni_surface_controls_plusminus", &controls.plusminus);
        }
        attrs
    }
}

/// A surface's geometry in AFNI's RAI coordinates (see [`flip_xy`]).
#[derive(Debug, Clone, Copy)]
pub struct Geometry<'a> {
    /// Node coordinates.
    pub vertices: &'a [[f32; 3]],
    /// One normal per node.
    pub normals: &'a [[f32; 3]],
    /// Triangles, as indices into `vertices`.
    pub triangles: &'a [[u32; 3]],
}

/// Negate x and y: RAS (GIfTI, NIfTI) ⇄ AFNI's RAI.
pub fn flip_xy([x, y, z]: [f32; 3]) -> [f32; 3] {
    [-x, -y, z]
}

fn float_columns(rows: &[[f32; 3]]) -> Vec<TypedArray> {
    (0..3)
        .map(|c| TypedArray::Float32(rows.iter().map(|r| r[c]).collect()))
        .collect()
}

impl Geometry<'_> {
    fn check(&self) -> Result<()> {
        if self.normals.len() != self.vertices.len() {
            return Err(Error::invalid(format!(
                "{} normals for {} vertices",
                self.normals.len(),
                self.vertices.len()
            )));
        }
        if i32::try_from(self.vertices.len()).is_err() {
            return Err(Error::invalid("too many vertices for a NIML int index"));
        }
        let n = self.vertices.len();
        if let Some(bad) = self.triangles.iter().flatten().find(|i| **i as usize >= n) {
            return Err(Error::invalid(format!(
                "triangle index {bad} is past the {n} vertices"
            )));
        }
        if self
            .triangles
            .iter()
            .flatten()
            .any(|i| i32::try_from(*i).is_err())
        {
            return Err(Error::invalid("triangle index exceeds i32::MAX"));
        }
        Ok(())
    }
}

/// `SUMA_ixyz`, `SUMA_node_normals` and `SUMA_ijk`, in the order AFNI needs
/// them: it refuses a `SUMA_ijk` that arrives before its `SUMA_ixyz`.
pub fn surface_elements(info: &SurfaceInfo, geometry: &Geometry<'_>) -> Result<[NimlElement; 3]> {
    geometry.check()?;
    let attrs = info.attributes();

    let mut ixyz = vec![TypedArray::Int32(
        (0..geometry.vertices.len() as i32).collect(),
    )];
    ixyz.extend(float_columns(geometry.vertices));
    let ixyz = NimlElement::numeric(
        "SUMA_ixyz",
        attrs.clone(),
        NumericMatrix::from_columns(ixyz)?,
    );

    let normals = NimlElement::numeric(
        "SUMA_node_normals",
        attrs.clone(),
        NumericMatrix::from_columns(float_columns(geometry.normals))?,
    );

    let ijk = (0..3)
        .map(|c| TypedArray::Int32(geometry.triangles.iter().map(|t| t[c] as i32).collect()))
        .collect();
    let ijk = NimlElement::numeric("SUMA_ijk", attrs, NumericMatrix::from_columns(ijk)?);

    Ok([ixyz, normals, ijk])
}

/// The bytes that register a surface: [`KEEP_READING`], the three
/// [`surface_elements`], then [`PAUSE_READING`].
pub fn registration_bytes(info: &SurfaceInfo, geometry: &Geometry<'_>) -> Result<Vec<u8>> {
    let elements = surface_elements(info, geometry)?;
    let mut out = KEEP_READING.to_vec();
    out.extend(encode(&elements));
    out.extend_from_slice(PAUSE_READING);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Crosshair
// ---------------------------------------------------------------------------

/// A `SUMA_crosshair_xyz` element: where the crosshair is, and what it is on.
///
/// SUMA sends one to AFNI as a bare element; AFNI sends SUMA a
/// [`CrosshairMessage`] group that holds one. AFNI only reads `position`
/// (and `icor`); SUMA also reads `surface_idcode` and `node_index`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Crosshair {
    /// RAI position (x, y, z).
    pub position: [f32; 3],
    /// `surface_idcode`.
    pub surface_idcode: Option<String>,
    /// `surface_label`.
    pub surface_label: Option<String>,
    /// `surface_nodeid`: the selected node.
    pub node_index: Option<u32>,
    /// `domain_parent_idcode`.
    pub domain_parent_idcode: Option<String>,
    /// `volume_idcode`.
    pub volume_idcode: Option<String>,
    /// `current_overlay_dset_id` (SUMA sends the overlay dataset's idcode).
    pub overlay_dset_id: Option<String>,
    /// `current_overlay_dset_filename`.
    pub overlay_dset_filename: Option<String>,
    /// `Do_icor`: also seed InstaCorr at this point.
    pub icor: bool,
}

impl Crosshair {
    /// A crosshair at `position` on `node_index` of `info`'s surface.
    pub fn on_surface(info: &SurfaceInfo, node_index: u32, position: [f32; 3]) -> Self {
        Self {
            position,
            surface_idcode: Some(info.surface_idcode.clone()),
            surface_label: Some(info.surface_label.clone()),
            node_index: Some(node_index),
            domain_parent_idcode: Some(info.local_domain_parent_id.clone()),
            volume_idcode: info.volume_idcode.clone(),
            ..Self::default()
        }
    }

    /// Build the `SUMA_crosshair_xyz` element.
    pub fn to_element(&self) -> NimlElement {
        let mut attrs = BTreeMap::new();
        let mut put = |key: &str, value: Option<&str>| {
            if let Some(value) = value {
                attrs.insert(key.to_string(), value.to_string());
            }
        };
        put("surface_idcode", self.surface_idcode.as_deref());
        put("surface_label", self.surface_label.as_deref());
        put("domain_parent_idcode", self.domain_parent_idcode.as_deref());
        put("volume_idcode", self.volume_idcode.as_deref());
        put("current_overlay_dset_id", self.overlay_dset_id.as_deref());
        put(
            "current_overlay_dset_filename",
            self.overlay_dset_filename.as_deref(),
        );
        if let Some(node) = self.node_index {
            attrs.insert("surface_nodeid".to_string(), node.to_string());
        }
        if self.icor {
            attrs.insert("Do_icor".to_string(), "yes".to_string());
        }
        let column = TypedArray::Float32(self.position.to_vec());
        let matrix = NumericMatrix::from_columns(vec![column]).expect("one float column");
        NimlElement::numeric("SUMA_crosshair_xyz", attrs, matrix)
    }

    /// Read a `SUMA_crosshair_xyz`: one column of exactly three floats.
    /// `surface_nodeid` may be empty or negative, which means no node.
    pub fn from_element(element: &NimlElement) -> Result<Self> {
        if element.name != "SUMA_crosshair_xyz" {
            return Err(Error::invalid(format!(
                "expected SUMA_crosshair_xyz, found {}",
                element.name
            )));
        }
        let NimlData::Numeric(matrix) = &element.data else {
            return Err(Error::invalid("SUMA_crosshair_xyz has no numeric body"));
        };
        let Some(TypedArray::Float32(values)) = matrix.column(0) else {
            return Err(Error::invalid("SUMA_crosshair_xyz needs a float column"));
        };
        if matrix.column_count() != 1 || values.len() != 3 {
            return Err(Error::invalid(
                "SUMA_crosshair_xyz requires 3 floats in one vector",
            ));
        }
        let get = |key: &str| {
            element
                .attrs
                .get(key)
                .map(String::as_str)
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
        };
        Ok(Self {
            position: [values[0], values[1], values[2]],
            surface_idcode: get("surface_idcode"),
            surface_label: get("surface_label"),
            node_index: get("surface_nodeid")
                .and_then(|v| v.trim().parse::<i64>().ok())
                .and_then(|v| u32::try_from(v).ok()),
            domain_parent_idcode: get("domain_parent_idcode"),
            volume_idcode: get("volume_idcode"),
            overlay_dset_id: get("current_overlay_dset_id"),
            overlay_dset_filename: get("current_overlay_dset_filename"),
            icor: element.attrs.contains_key("Do_icor"),
        })
    }
}

/// The underlay values at the crosshair, from AFNI (`underlay_array`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UnderlayArray {
    /// `underlay_idcode`.
    pub idcode: Option<String>,
    /// `vox_ijk`: the voxel's indices.
    pub voxel_ijk: Option<[i32; 3]>,
    /// `has_taxis`: whether the underlay is a time series.
    pub has_time_axis: Option<bool>,
    /// The voxel's value in each sub-brick.
    pub values: Vec<f32>,
}

/// What AFNI sends SUMA when its crosshair moves: a `SUMA_crosshair` group of
/// the position, the underlay values there, and (when the crosshair is on a
/// surface node) the underlay time series mapped to that node.
#[derive(Debug, Clone, PartialEq)]
pub struct CrosshairMessage {
    /// The `SUMA_crosshair_xyz` child.
    pub crosshair: Crosshair,
    /// The `underlay_array` child, if present.
    pub underlay: Option<UnderlayArray>,
    /// The values of the `v2s_node_array` child (volume-to-surface time
    /// series at the node), if present.
    pub v2s_node_values: Option<Vec<f32>>,
}

impl CrosshairMessage {
    /// Read a `SUMA_crosshair` group.
    pub fn from_element(group: &NimlElement) -> Result<Self> {
        if group.name != "SUMA_crosshair" {
            return Err(Error::invalid(format!(
                "expected a SUMA_crosshair group, found {}",
                group.name
            )));
        }
        let xyz = group
            .child("SUMA_crosshair_xyz")
            .ok_or_else(|| Error::missing("SUMA_crosshair_xyz in SUMA_crosshair"))?;
        let underlay = group.child("underlay_array").map(|e| UnderlayArray {
            idcode: e.attrs.get("underlay_idcode").cloned(),
            voxel_ijk: e.attrs.get("vox_ijk").and_then(|v| {
                let mut it = v.split_whitespace().map(|n| n.parse::<i32>());
                Some([it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?])
            }),
            has_time_axis: e
                .attrs
                .get("has_taxis")
                .map(|v| v.eq_ignore_ascii_case("y")),
            values: float_values(e),
        });
        let v2s_node_values = group.child("v2s_node_array").map(float_values);
        Ok(Self {
            crosshair: Crosshair::from_element(xyz)?,
            underlay,
            v2s_node_values,
        })
    }

    /// Build the `SUMA_crosshair` group, as AFNI does.
    pub fn to_element(&self) -> NimlElement {
        let mut children = vec![self.crosshair.to_element()];
        if let Some(underlay) = &self.underlay {
            let mut attrs = BTreeMap::new();
            if let Some(id) = &underlay.idcode {
                attrs.insert("underlay_idcode".to_string(), id.clone());
            }
            if let Some([i, j, k]) = underlay.voxel_ijk {
                attrs.insert("vox_ijk".to_string(), format!("{i} {j} {k}"));
            }
            if let Some(taxis) = underlay.has_time_axis {
                attrs.insert(
                    "has_taxis".to_string(),
                    if taxis { "y" } else { "n" }.into(),
                );
            }
            children.push(float_element("underlay_array", attrs, &underlay.values));
        }
        if let Some(values) = &self.v2s_node_values {
            let mut attrs = BTreeMap::new();
            if let Some(node) = self.crosshair.node_index {
                attrs.insert("surface_nodeid".to_string(), node.to_string());
            }
            children.push(float_element("v2s_node_array", attrs, values));
        }
        NimlElement::group("SUMA_crosshair", BTreeMap::new(), children)
    }
}

fn float_values(element: &NimlElement) -> Vec<f32> {
    match &element.data {
        NimlData::Numeric(m) => (0..m.rows)
            .filter_map(|row| m.get(row, 0))
            .map(|v| v as f32)
            .collect(),
        _ => Vec::new(),
    }
}

fn float_element(name: &str, attrs: BTreeMap<String, String>, values: &[f32]) -> NimlElement {
    if values.is_empty() {
        return NimlElement {
            name: name.to_string(),
            attrs,
            data: NimlData::None,
        };
    }
    let column = TypedArray::Float32(values.to_vec());
    let matrix = NumericMatrix::from_columns(vec![column]).expect("one float column");
    NimlElement::numeric(name, attrs, matrix)
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// A `SUMA_irgba` element: the colour AFNI gives each node of a surface for
/// its current functional overlay. An element with no nodes means "no
/// overlay".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Irgba {
    /// `surface_idcode`. Required.
    pub surface_idcode: String,
    /// `local_domain_parent_ID`.
    pub local_domain_parent_id: Option<String>,
    /// `volume_idcode`: the underlay.
    pub volume_idcode: Option<String>,
    /// `function_idcode`: the overlay.
    pub function_idcode: Option<String>,
    /// `threshold`, as AFNI wrote it.
    pub threshold: Option<String>,
    /// `numvox_total`.
    pub numvox_total: Option<i64>,
    /// Node indices.
    pub nodes: Vec<u32>,
    /// One RGBA colour per node.
    pub rgba: Vec<[u8; 4]>,
}

impl Irgba {
    /// Read a `SUMA_irgba` element (`int,4*byte`). An element with no rows,
    /// or no body at all, is read as empty.
    pub fn from_element(element: &NimlElement) -> Result<Self> {
        if element.name != "SUMA_irgba" {
            return Err(Error::invalid(format!(
                "expected SUMA_irgba, found {}",
                element.name
            )));
        }
        let get = |key: &str| element.attrs.get(key).cloned();
        let surface_idcode = get("surface_idcode")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::missing("surface_idcode on SUMA_irgba"))?;
        let (nodes, rgba) = match &element.data {
            NimlData::None => (Vec::new(), Vec::new()),
            NimlData::Text(text) if text.trim().is_empty() => (Vec::new(), Vec::new()),
            NimlData::Numeric(m) if m.rows == 0 => (Vec::new(), Vec::new()),
            NimlData::Numeric(m) => {
                if m.column_count() < 5 {
                    return Err(Error::invalid("SUMA_irgba needs node, r, g, b, a columns"));
                }
                let mut nodes = Vec::with_capacity(m.rows);
                let mut rgba = Vec::with_capacity(m.rows);
                for row in 0..m.rows {
                    let at = |c: usize| m.get(row, c).unwrap_or(0.0);
                    let node = at(0).round();
                    if node < 0.0 {
                        return Err(Error::invalid("SUMA_irgba node index is negative"));
                    }
                    nodes.push(node as u32);
                    rgba.push([1, 2, 3, 4].map(|c| at(c).round().clamp(0.0, 255.0) as u8));
                }
                (nodes, rgba)
            }
            _ => {
                return Err(Error::invalid(
                    "SUMA_irgba must hold a numeric matrix or be empty",
                ))
            }
        };
        Ok(Self {
            surface_idcode,
            local_domain_parent_id: get("local_domain_parent_ID"),
            volume_idcode: get("volume_idcode"),
            function_idcode: get("function_idcode"),
            threshold: get("threshold"),
            numvox_total: get("numvox_total").and_then(|v| v.trim().parse().ok()),
            nodes,
            rgba,
        })
    }

    /// Build the element (`ni_type="int,4*byte"`; with no nodes it declares
    /// the type and `ni_dimen="0"`).
    pub fn to_element(&self) -> Result<NimlElement> {
        if self.nodes.len() != self.rgba.len() {
            return Err(Error::invalid(format!(
                "{} nodes but {} colours",
                self.nodes.len(),
                self.rgba.len()
            )));
        }
        let mut attrs = BTreeMap::new();
        attrs.insert("surface_idcode".to_string(), self.surface_idcode.clone());
        for (key, value) in [
            ("local_domain_parent_ID", &self.local_domain_parent_id),
            ("volume_idcode", &self.volume_idcode),
            ("function_idcode", &self.function_idcode),
            ("threshold", &self.threshold),
        ] {
            if let Some(value) = value {
                attrs.insert(key.to_string(), value.clone());
            }
        }
        if let Some(total) = self.numvox_total {
            attrs.insert("numvox_total".to_string(), total.to_string());
        }
        let mut columns = vec![TypedArray::Int32(
            self.nodes
                .iter()
                .map(|n| i32::try_from(*n))
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| Error::invalid("node index exceeds i32::MAX"))?,
        )];
        for c in 0..4 {
            columns.push(TypedArray::UInt8(self.rgba.iter().map(|p| p[c]).collect()));
        }
        Ok(NimlElement::numeric(
            "SUMA_irgba",
            attrs,
            NumericMatrix::from_columns(columns)?,
        ))
    }
}
