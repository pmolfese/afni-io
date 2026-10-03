//! SUMA surface specification files (`.spec`).
//!
//! A spec file groups the surfaces belonging to a subject. It is a flat,
//! line-oriented `Key = Value` format. Each surface begins with a `NewSurface`
//! line followed by its fields; comments start with `#`. Common keys include
//! `SurfaceName`, `SurfaceType`, `SurfaceState`, `Anatomical`, `LocalDomainParent`,
//! and `LocalCurvatureParent`.
//!
//! [`Spec::parse`] and [`Spec::read`] keep every field as written. What SUMA
//! makes of them (inherited values, defaults, path prefixes, the checks it
//! applies) is [`Spec::resolve`], which follows `SUMA_Read_SpecFile` and
//! `SUMA_CheckOnSpecFile`; its results match `inspec -detail 3`. [`Spec::to_text`]
//! and [`Spec::write`] write a spec the way `SUMA_Write_SpecFile` does.
//!
//! Where this differs from SUMA, on purpose: a field name must match exactly
//! (SUMA tests whether the line merely *contains* it), and a `SurfaceState`
//! must be one of the declared states (SUMA also accepts one that is a part of
//! the list's text).
//!
//! Reference: `afni/src/SUMA/SUMA_Load_Surface_Object.c`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{self, from_utf8, Error, Result};

/// One surface entry from a `.spec` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecSurface {
    /// All `Key = Value` fields for this surface, in file order.
    pub fields: Vec<(String, String)>,
}

impl SpecSurface {
    /// Look up a field value by key (case-sensitive, as SUMA writes them).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `SurfaceName`: the surface's file name (relative to the spec file).
    pub fn surface_name(&self) -> Option<&str> {
        self.get("SurfaceName")
            .or_else(|| self.get("FreeSurferSurface"))
    }

    /// Resolve the surface file path relative to the spec file's directory.
    pub fn resolve_path(&self, spec_dir: &Path) -> Option<PathBuf> {
        self.surface_name().map(|name| spec_dir.join(name))
    }

    /// `SurfaceState`: the geometric state, e.g. `pial`, `smoothwm`.
    pub fn state(&self) -> Option<&str> {
        self.get("SurfaceState")
    }

    /// Whether `Anatomical = Y`.
    pub fn anatomical(&self) -> Option<bool> {
        self.get("Anatomical").map(|v| v.eq_ignore_ascii_case("y"))
    }
}

/// A parsed `.spec` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spec {
    /// Global state keys declared outside any surface block (e.g. `StateDef`).
    pub globals: BTreeMap<String, Vec<String>>,
    /// The surfaces, in file order.
    pub surfaces: Vec<SpecSurface>,
}

impl Spec {
    /// Read and parse a `.spec` file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = error::read_file(path.as_ref())?;
        Self::parse(&from_utf8(&bytes, "spec file")?)
    }

    /// Parse spec-file text.
    pub fn parse(text: &str) -> Result<Self> {
        let mut spec = Spec::default();
        let mut current: Option<Vec<(String, String)>> = None;

        for raw in text.lines() {
            let line = raw.trim();
            // SUMA drops any line that has a `#` anywhere in it, not just a
            // line that starts with one.
            if line.is_empty() || line.contains('#') {
                continue;
            }
            // `NewSurface` opens a block; a fresh one closes the previous.
            if line.eq_ignore_ascii_case("NewSurface") {
                if let Some(fields) = current.take() {
                    if !fields.is_empty() {
                        spec.surfaces.push(SpecSurface { fields });
                    }
                }
                current = Some(Vec::new());
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(Error::parse(format!(
                    "spec file has a line that is not `Key = Value`: {line}"
                )));
            };
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            match &mut current {
                Some(fields) => fields.push((key, value)),
                None => spec.globals.entry(key).or_default().push(value),
            }
        }
        if let Some(fields) = current.take() {
            if !fields.is_empty() {
                spec.surfaces.push(SpecSurface { fields });
            }
        }
        Ok(spec)
    }

    /// Resolve every surface file path relative to `spec_path`'s directory.
    pub fn surface_paths(&self, spec_path: impl AsRef<Path>) -> Vec<PathBuf> {
        let dir = spec_path
            .as_ref()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        self.surfaces
            .iter()
            .filter_map(|s| s.resolve_path(&dir))
            .collect()
    }
}

/// The hemisphere a spec entry declares (`Hemisphere = L|R|B`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hemisphere {
    /// `L`.
    Left,
    /// `R`.
    Right,
    /// `B`: both.
    Both,
}

impl Hemisphere {
    fn from_letter(s: &str) -> Option<Self> {
        match s {
            "L" => Some(Self::Left),
            "R" => Some(Self::Right),
            "B" => Some(Self::Both),
            _ => None,
        }
    }

    fn letter(self) -> &'static str {
        match self {
            Self::Left => "L",
            Self::Right => "R",
            Self::Both => "B",
        }
    }
}

/// One surface as SUMA understands it after reading a spec file: the fields
/// it inherits, its defaults, and the spec file's directory put in front of
/// every file name (see [`Spec::resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSurface {
    /// `SurfaceFormat` (`ASCII` unless stated).
    pub format: String,
    /// `SurfaceType` (`FreeSurfer`, `GIFTI`, `SureFit`, …).
    pub surface_type: String,
    /// `SurfaceState`.
    pub state: String,
    /// `Group`.
    pub group: String,
    /// `EmbedDimension` (2 or 3; 3 unless stated).
    pub embed_dim: u8,
    /// `SurfaceName` (or `FreeSurferSurface` or `InventorSurface`), with the
    /// spec directory in front.
    pub surface_file: Option<String>,
    /// `CoordFile` (or `SureFitCoord`), with the spec directory in front.
    pub coord_file: Option<String>,
    /// `TopoFile` (or `SureFitTopo`), with the spec directory in front.
    pub topo_file: Option<String>,
    /// `SureFitVolParam`, with the spec directory in front.
    pub surefit_vol_param: Option<String>,
    /// `LocalDomainParent`, with the spec directory in front. `SAME` becomes
    /// `<dir>SAME`, and so does a parent that is the surface itself.
    pub local_domain_parent: Option<String>,
    /// `LocalCurvatureParent`; the domain parent when not stated.
    pub local_curvature_parent: Option<String>,
    /// `LabelDset`, with the spec directory in front.
    pub label_dset: Option<String>,
    /// `NodeMarker`, with the spec directory in front.
    pub node_marker: Option<String>,
    /// `SurfaceVolume`, as written (SUMA wants a path that works from where it
    /// runs).
    pub volume: Option<String>,
    /// `SurfaceLabel`.
    pub label: Option<String>,
    /// `Anatomical`: `Y` is `true`, `N` is `false`.
    pub anatomical: Option<bool>,
    /// `Hemisphere`.
    pub hemisphere: Option<Hemisphere>,
    /// `DomainGrandParentID`.
    pub domain_grandparent_id: Option<String>,
    /// `OriginatorID`.
    pub originator_id: Option<String>,
}

impl ResolvedSurface {
    /// A new entry for a surface file, with SUMA's defaults (`ASCII`, 3
    /// dimensions) and no other fields. `name` is written as given.
    pub fn new(
        surface_type: impl Into<String>,
        name: impl Into<String>,
        state: impl Into<String>,
    ) -> Self {
        Self {
            format: "ASCII".into(),
            surface_type: surface_type.into(),
            state: state.into(),
            group: String::new(),
            embed_dim: 3,
            surface_file: Some(name.into()),
            coord_file: None,
            topo_file: None,
            surefit_vol_param: None,
            local_domain_parent: None,
            local_curvature_parent: None,
            label_dset: None,
            node_marker: None,
            volume: None,
            label: None,
            anatomical: None,
            hemisphere: None,
            domain_grandparent_id: None,
            originator_id: None,
        }
    }

    /// The surface's file, or its coordinate file when the surface is split.
    pub fn file(&self) -> Option<&str> {
        self.surface_file.as_deref().or(self.coord_file.as_deref())
    }
}

/// The directory prefix SUMA puts in front of every file name: the spec's
/// directory with a trailing `/`, or `./` when the spec is named without one.
/// (SUMA does this for an absolute name too, giving `dir//abs/name`.)
fn spec_prefix(spec_path: &Path) -> String {
    let text = spec_path.to_string_lossy();
    match text.rfind('/') {
        Some(i) => text[..=i].to_string(),
        None => "./".to_string(),
    }
}

/// Everything one `NewSurface` block can set, as SUMA keeps it while reading.
#[derive(Debug, Clone, Default)]
struct RawSurface {
    format: String,
    surface_type: String,
    state: String,
    group: String,
    embed_dim: u8,
    surface_file: String,
    coord_file: String,
    topo_file: String,
    mapping_ref: String,
    surefit_vol_param: String,
    ldp: String,
    lcp: String,
    label_dset: String,
    node_marker: String,
    volume: String,
    label: String,
    anatomical: String,
    hemisphere: String,
    dgp: String,
    originator: String,
}

/// The fields a block may name, and which "may be given once" slot each uses.
/// SUMA lets `TopoFile` and `SureFitTopo` (and the coordinate and surface-file
/// pairs) share a slot.
fn slot(key: &str) -> Option<&'static str> {
    Some(match key {
        "Group" => "Group",
        "Anatomical" => "Anatomical",
        "Hemisphere" => "Hemisphere",
        "DomainGrandParentID" => "DomainGrandParentID",
        "OriginatorID" => "OriginatorID",
        "LocalCurvatureParent" => "LocalCurvatureParent",
        "LocalDomainParent" => "LocalDomainParent",
        "LabelDset" => "LabelDset",
        "NodeMarker" => "NodeMarker",
        "EmbedDimension" => "EmbedDimension",
        "SurfaceState" => "SurfaceState",
        "SurfaceFormat" => "SurfaceFormat",
        "SurfaceType" => "SurfaceType",
        "TopoFile" | "SureFitTopo" => "TopoFile",
        "CoordFile" | "SureFitCoord" => "CoordFile",
        "MappingRef" => "MappingRef",
        "SureFitVolParam" => "SureFitVolParam",
        "FreeSurferSurface" | "SurfaceName" | "InventorSurface" => "SurfaceFile",
        "SurfaceVolume" => "SurfaceVolume",
        "SurfaceLabel" => "SurfaceLabel",
        _ => return None,
    })
}

fn bad(msg: impl Into<String>) -> Error {
    Error::parse(msg.into())
}

impl Spec {
    /// The declared states (`StateDef`), in order.
    pub fn states(&self) -> Vec<&str> {
        let mut states: Vec<&str> = self
            .globals
            .get("StateDef")
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        for surface in &self.surfaces {
            states.extend(
                surface
                    .fields
                    .iter()
                    .filter(|(k, _)| k == "StateDef")
                    .map(|(_, v)| v.as_str()),
            );
        }
        states
    }

    /// Read the spec the way SUMA does and return each surface as it ends up:
    /// inheriting from the surface before it, with defaults, with the spec's
    /// directory in front of file names, and after SUMA's checks. `spec_path`
    /// is the spec file's own path (only its directory is used).
    ///
    /// * A surface inherits `SurfaceFormat`, `SurfaceType`, `TopoFile`,
    ///   `SureFitVolParam`, `MappingRef`, `Group`, `SurfaceState` and
    ///   `EmbedDimension` from the one before it. The first has format `ASCII`
    ///   and 3 dimensions. Nothing else is inherited.
    /// * `MappingRef` is the obsolete form of `LocalDomainParent` and
    ///   `LocalCurvatureParent`; it cannot be mixed with them, `OriginatorID`
    ///   or `DomainGrandParentID`.
    /// * `LocalCurvatureParent` defaults to the domain parent, and must contain
    ///   its name if given. A domain parent that is the surface's own file
    ///   becomes `SAME`.
    ///
    /// It fails where SUMA fails: a surface with no `SurfaceType`; a field
    /// given twice in one block, or before the first `NewSurface`; a name it
    /// does not know; `Anatomical` other than `Y`/`N`; `Hemisphere` other than
    /// `L`/`R`/`B`; `EmbedDimension` other than 2 or 3; a `SurfaceState` that
    /// was not declared by `StateDef`.
    pub fn resolve(&self, spec_path: impl AsRef<Path>) -> Result<Vec<ResolvedSurface>> {
        let prefix = spec_prefix(spec_path.as_ref());
        let joined = |name: &str| format!("{prefix}{name}");

        let mut states: Vec<String> = Vec::new();
        let mut group0 = String::new();
        let mut group_seen = false;
        for (key, values) in &self.globals {
            for value in values {
                match key.as_str() {
                    "StateDef" => states.push(value.clone()),
                    "Group" => {
                        if group_seen {
                            return Err(bad("Group is given twice"));
                        }
                        group_seen = true;
                        group0 = value.clone();
                    }
                    other if slot(other).is_some() => {
                        return Err(bad(format!(
                            "{other} comes before the first NewSurface line"
                        )))
                    }
                    other => return Err(bad(format!("unknown spec field {other}"))),
                }
            }
        }

        let mut raws: Vec<RawSurface> = Vec::new();
        for (index, block) in self.surfaces.iter().enumerate() {
            let mut raw = match raws.last() {
                None => RawSurface {
                    format: "ASCII".into(),
                    embed_dim: 3,
                    group: group0.clone(),
                    ..RawSurface::default()
                },
                Some(previous) => {
                    if previous.surface_type.is_empty() {
                        return Err(bad(format!("no SurfaceType for surface {}", index - 1)));
                    }
                    RawSurface {
                        format: previous.format.clone(),
                        surface_type: previous.surface_type.clone(),
                        topo_file: previous.topo_file.clone(),
                        mapping_ref: previous.mapping_ref.clone(),
                        surefit_vol_param: previous.surefit_vol_param.clone(),
                        group: previous.group.clone(),
                        state: previous.state.clone(),
                        embed_dim: previous.embed_dim,
                        ..RawSurface::default()
                    }
                }
            };
            let mut seen: Vec<&'static str> = Vec::new();
            for (key, value) in &block.fields {
                if key == "StateDef" {
                    states.push(value.clone());
                    continue;
                }
                let Some(name) = slot(key) else {
                    return Err(bad(format!("unknown spec field {key}")));
                };
                if seen.contains(&name) {
                    return Err(bad(format!("{name} is given twice for surface {index}")));
                }
                seen.push(name);
                match key.as_str() {
                    "Group" => raw.group = value.clone(),
                    "Anatomical" => {
                        if value != "Y" && value != "N" {
                            return Err(bad("Anatomical can only be Y or N"));
                        }
                        raw.anatomical = value.clone();
                    }
                    "Hemisphere" => {
                        if Hemisphere::from_letter(value).is_none() {
                            return Err(bad("Hemisphere can only be L or R or B"));
                        }
                        raw.hemisphere = value.clone();
                    }
                    "DomainGrandParentID" => raw.dgp = value.clone(),
                    "OriginatorID" => raw.originator = value.clone(),
                    "LocalCurvatureParent" => raw.lcp = joined(value),
                    "LocalDomainParent" => raw.ldp = joined(value),
                    "LabelDset" => raw.label_dset = joined(value),
                    "NodeMarker" => raw.node_marker = joined(value),
                    "EmbedDimension" => {
                        raw.embed_dim = match value.trim().parse::<u8>() {
                            Ok(n @ (2 | 3)) => n,
                            _ => return Err(bad("EmbedDimension can only be 2 or 3")),
                        }
                    }
                    "SurfaceState" => {
                        if !states.iter().any(|s| s == value) {
                            return Err(bad(format!("state {value} was not declared by StateDef")));
                        }
                        raw.state = value.clone();
                    }
                    "SurfaceFormat" => raw.format = value.clone(),
                    "SurfaceType" => raw.surface_type = value.clone(),
                    "TopoFile" | "SureFitTopo" => raw.topo_file = joined(value),
                    "CoordFile" | "SureFitCoord" => raw.coord_file = joined(value),
                    "MappingRef" => raw.mapping_ref = joined(value),
                    "SureFitVolParam" => raw.surefit_vol_param = joined(value),
                    "FreeSurferSurface" | "SurfaceName" | "InventorSurface" => {
                        raw.surface_file = joined(value)
                    }
                    "SurfaceVolume" => raw.volume = value.clone(),
                    "SurfaceLabel" => raw.label = value.clone(),
                    _ => unreachable!("slot() accepted {key}"),
                }
            }
            raws.push(raw);
        }
        if let Some(last) = raws.last() {
            if last.surface_type.is_empty() {
                return Err(bad(format!(
                    "no SurfaceType for surface {}",
                    raws.len() - 1
                )));
            }
        }

        // SUMA_CheckOnSpecFile, once everything is read.
        let some = |s: &String| (!s.is_empty()).then(|| s.clone());
        raws.into_iter()
            .map(|mut raw| {
                if !raw.mapping_ref.is_empty()
                    && (!raw.ldp.is_empty()
                        || !raw.lcp.is_empty()
                        || !raw.originator.is_empty()
                        || !raw.dgp.is_empty())
                {
                    return Err(bad("MappingRef cannot be mixed with LocalDomainParent, \
                         LocalCurvatureParent, OriginatorID or DomainGrandParentID"));
                }
                if !raw.mapping_ref.is_empty() {
                    raw.ldp = std::mem::take(&mut raw.mapping_ref);
                    raw.lcp = raw.ldp.clone();
                }
                if raw.lcp.is_empty() {
                    raw.lcp = raw.ldp.clone();
                } else if !raw.lcp.contains(&raw.ldp) {
                    return Err(bad(
                        "LocalCurvatureParent and LocalDomainParent must be identical",
                    ));
                }
                if !raw.ldp.is_empty() && raw.ldp == raw.surface_file {
                    raw.ldp = joined("SAME");
                }
                Ok(ResolvedSurface {
                    format: raw.format,
                    surface_type: raw.surface_type,
                    state: raw.state,
                    group: raw.group,
                    embed_dim: raw.embed_dim,
                    surface_file: some(&raw.surface_file),
                    coord_file: some(&raw.coord_file),
                    topo_file: some(&raw.topo_file),
                    surefit_vol_param: some(&raw.surefit_vol_param),
                    local_domain_parent: some(&raw.ldp),
                    local_curvature_parent: some(&raw.lcp),
                    label_dset: some(&raw.label_dset),
                    node_marker: some(&raw.node_marker),
                    volume: some(&raw.volume),
                    label: some(&raw.label),
                    anatomical: match raw.anatomical.as_str() {
                        "" => None,
                        a => Some(a == "Y"),
                    },
                    hemisphere: Hemisphere::from_letter(&raw.hemisphere),
                    domain_grandparent_id: some(&raw.dgp),
                    originator_id: some(&raw.originator),
                })
            })
            .collect()
    }

    /// A spec holding `surfaces`, laid out the way SUMA writes one
    /// (`SUMA_Write_SpecFile`): the same field order, `StateDef`s for each
    /// distinct state in order of first use, `LocalDomainParent = SAME` when
    /// there is none, and `Anatomical`, `Hemisphere` and the like only when set.
    /// Names are written as given.
    ///
    /// SUMA writes a single group, so every surface's `group` must equal
    /// `group` or be empty.
    pub fn from_resolved(group: &str, surfaces: &[ResolvedSurface]) -> Result<Self> {
        let mut spec = Spec::default();
        spec.globals.insert("Group".into(), vec![group.to_string()]);
        let mut states: Vec<String> = Vec::new();
        for s in surfaces {
            if !s.group.is_empty() && s.group != group {
                return Err(Error::invalid(format!(
                    "SUMA reads one group per spec: {} is not {group}",
                    s.group
                )));
            }
            if !states.contains(&s.state) {
                states.push(s.state.clone());
            }
        }
        spec.globals.insert("StateDef".into(), states);

        for s in surfaces {
            let mut f: Vec<(String, String)> = Vec::new();
            let mut put = |k: &str, v: &str| f.push((k.to_string(), v.to_string()));
            put("SurfaceFormat", &s.format);
            put("SurfaceType", &s.surface_type);
            match &s.surface_file {
                Some(name) => put("SurfaceName", name),
                None => {
                    put("CoordFile", s.coord_file.as_deref().unwrap_or(""));
                    put("TopoFile", s.topo_file.as_deref().unwrap_or(""));
                }
            }
            if let Some(v) = &s.surefit_vol_param {
                put("SureFitVolParam", v);
            }
            put(
                "LocalDomainParent",
                s.local_domain_parent.as_deref().unwrap_or("SAME"),
            );
            if let Some(v) = &s.label_dset {
                put("LabelDset", v);
            }
            if let Some(v) = &s.node_marker {
                put("NodeMarker", v);
            }
            put("SurfaceState", &s.state);
            put("EmbedDimension", &s.embed_dim.to_string());
            if let Some(v) = &s.volume {
                put("SurfaceVolume", v);
            }
            if let Some(v) = &s.label {
                put("SurfaceLabel", v);
            }
            if let Some(a) = s.anatomical {
                put("Anatomical", if a { "Y" } else { "N" });
            }
            if let Some(h) = s.hemisphere {
                put("Hemisphere", h.letter());
            }
            if let Some(v) = &s.domain_grandparent_id {
                put("DomainGrandParentID", v);
            }
            if let Some(v) = &s.originator_id {
                put("OriginatorID", v);
            }
            if let Some(v) = &s.local_curvature_parent {
                put("LocalCurvatureParent", v);
            }
            spec.surfaces.push(SpecSurface { fields: f });
        }
        Ok(spec)
    }

    /// The spec as text, laid out as `SUMA_Write_SpecFile` lays it out: an
    /// optional `# <program> generated spec file` line and `#History:` line, then
    /// `Group`, the `StateDef`s, and each surface after a `NewSurface` line with
    /// its fields in the order held. A value with a `#` in it cannot be written
    /// back, because SUMA reads any line with a `#` as a comment.
    pub fn to_text(&self, program: Option<&str>, history: Option<&str>) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        if let Some(program) = program.filter(|p| !p.is_empty()) {
            let _ = writeln!(out, "# {program} generated spec file");
        }
        match history {
            Some(note) => {
                let _ = write!(out, "#History: {note}\n\n");
            }
            None => out.push('\n'),
        }
        if let Some(group) = self.globals.get("Group").and_then(|g| g.first()) {
            let _ = write!(out, "#define the group\n\tGroup = {group}\n\n");
        }
        out.push_str("#define various States\n");
        for state in self.globals.get("StateDef").into_iter().flatten() {
            let _ = writeln!(out, "\tStateDef = {state}");
        }
        for (key, values) in &self.globals {
            if key != "Group" && key != "StateDef" {
                for value in values {
                    let _ = writeln!(out, "\t{key} = {value}");
                }
            }
        }
        for surface in &self.surfaces {
            out.push_str("\nNewSurface\n");
            for (key, value) in &surface.fields {
                let _ = writeln!(out, "\t{key} = {value}");
            }
        }
        out
    }

    /// Write [`Spec::to_text`] to `path`.
    pub fn write(
        &self,
        path: impl AsRef<Path>,
        program: Option<&str>,
        history: Option<&str>,
    ) -> Result<()> {
        error::write_file(path.as_ref(), self.to_text(program, history).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# define the spec file
StateDef = smoothwm
StateDef = pial

NewSurface
        SurfaceFormat = ASCII
        SurfaceType = FreeSurfer
        SurfaceName = lh.smoothwm.asc
        Anatomical = Y
        LocalDomainParent = SAME
        SurfaceState = smoothwm

NewSurface
        SurfaceFormat = ASCII
        SurfaceType = FreeSurfer
        SurfaceName = lh.pial.asc
        Anatomical = N
        SurfaceState = pial
"#;

    #[test]
    fn parses_surfaces() {
        let spec = Spec::parse(SAMPLE).unwrap();
        assert_eq!(spec.surfaces.len(), 2);
        assert_eq!(spec.surfaces[0].surface_name(), Some("lh.smoothwm.asc"));
        assert_eq!(spec.surfaces[0].state(), Some("smoothwm"));
        assert_eq!(spec.surfaces[0].anatomical(), Some(true));
        assert_eq!(spec.surfaces[1].anatomical(), Some(false));
        assert_eq!(
            spec.globals.get("StateDef").map(Vec::as_slice),
            Some(&["smoothwm".to_string(), "pial".to_string()][..])
        );
    }

    #[test]
    fn resolves_paths() {
        let spec = Spec::parse(SAMPLE).unwrap();
        let paths = spec.surface_paths("/data/subj/lh.spec");
        assert_eq!(paths[0], PathBuf::from("/data/subj/lh.smoothwm.asc"));
    }
}
