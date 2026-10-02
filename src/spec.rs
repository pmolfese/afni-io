//! SUMA surface specification files (`.spec`).
//!
//! A spec file groups the surfaces belonging to a subject. It is a flat,
//! line-oriented `Key = Value` format. Each surface begins with a `NewSurface`
//! line followed by its fields; comments start with `#`. Common keys include
//! `SurfaceName`, `SurfaceType`, `SurfaceState`, `Anatomical`, `LocalDomainParent`,
//! and `LocalCurvatureParent`.
//!
//! Reference: `SUMAvista/src/pysuma/io.py:parse_spec_surfaces`,
//! `afni/src/SUMA/SUMA_spec.c`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{self, from_utf8, Result};

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
            if line.is_empty() || line.starts_with('#') {
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
                continue;
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
