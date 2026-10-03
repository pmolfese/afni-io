//! `.spec` handling checked against AFNI's `inspec`: the fields it resolves
//! (`inspec -detail 3`), the file its writer makes (`inspec -prefix`), and the
//! files it refuses. The reference output is made by
//! `tests/data/make_spec_fixtures.sh`.

mod common;

use afni_io::spec::{Hemisphere, ResolvedSurface, Spec};

const GOOD: [&str; 5] = ["inherit", "split", "mapping", "same", "global_group"];
const BAD: [&str; 11] = [
    "bad_anatomical",
    "bad_before_newsurface",
    "bad_curvature",
    "bad_duplicate",
    "bad_embed",
    "bad_gibberish",
    "bad_hemisphere",
    "bad_mix_mapping",
    "bad_no_type",
    "bad_second_no_type",
    "bad_state_undefined",
];

/// One surface of `inspec -detail 3`: the names on its heading line, and its
/// `Field: value` lines (`(empty)` is an empty string).
struct InspecSurface {
    names: Vec<String>,
    fields: Vec<(String, String)>,
}

impl InspecSurface {
    fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("inspec has no {key}"))
            .1
            .as_str()
    }
}

fn inspec(name: &str) -> Vec<InspecSurface> {
    let text = std::fs::read_to_string(common::data(&format!("spec/{name}.inspec.txt"))).unwrap();
    let mut surfaces: Vec<InspecSurface> = Vec::new();
    for line in text.lines() {
        if let Some((head, rest)) = line.split_once(") ") {
            if head.chars().all(|c| c.is_ascii_digit()) && !head.is_empty() {
                surfaces.push(InspecSurface {
                    names: rest.split_whitespace().map(str::to_string).collect(),
                    fields: Vec::new(),
                });
                continue;
            }
        }
        if let (Some(surface), Some((key, value))) =
            (surfaces.last_mut(), line.trim().split_once(':'))
        {
            let value = value.trim();
            let value = if value == "(empty)" { "" } else { value };
            surface.fields.push((key.to_string(), value.to_string()));
        }
    }
    surfaces
}

fn resolved(name: &str) -> Vec<ResolvedSurface> {
    let spec = Spec::read(common::data(&format!("spec/{name}.spec"))).unwrap();
    // The reference output was made from a bare file name, so the prefix is `./`.
    spec.resolve(format!("{name}.spec")).unwrap()
}

fn opt(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("")
}

#[test]
fn resolved_fields_match_inspec() {
    for name in GOOD {
        let ours = resolved(name);
        let theirs = inspec(name);
        assert_eq!(ours.len(), theirs.len(), "{name}");
        for (i, (us, them)) in ours.iter().zip(&theirs).enumerate() {
            let at = format!("{name} surface {i}");
            // `inspec` shows file names only for some surface types (it shows
            // none for GIFTI, for one).
            let shown = [
                "SureFit",
                "1D",
                "FreeSurfer",
                "Ply",
                "GenericInventor",
                "OpenDX",
            ]
            .iter()
            .any(|t| us.surface_type.contains(t));
            let names: Vec<&str> = match (shown, us.surface_file.is_some()) {
                (false, _) => vec![],
                (true, true) => vec![opt(&us.surface_file)],
                (true, false) => vec![opt(&us.coord_file), opt(&us.topo_file)],
            };
            assert_eq!(names, them.names, "{at}: names");
            assert_eq!(us.surface_type, them.get("Type"), "{at}: Type");
            assert_eq!(us.format, them.get("Format"), "{at}: Format");
            assert_eq!(
                us.embed_dim.to_string(),
                them.get("EmbedDim"),
                "{at}: EmbedDim"
            );
            assert_eq!(
                format!("{}, Group {}", us.state, us.group).trim_end(),
                them.get("State"),
                "{at}: State"
            );
            assert_eq!(
                opt(&us.surefit_vol_param),
                them.get("SureFitVolParam"),
                "{at}"
            );
            assert_eq!(opt(&us.volume), them.get("VolParName"), "{at}: VolParName");
            let anatomical = match us.anatomical {
                None => "",
                Some(true) => "Y",
                Some(false) => "N",
            };
            assert_eq!(anatomical, them.get("AnatCorrect"), "{at}: Anatomical");
            let hemisphere = match us.hemisphere {
                None => "",
                Some(Hemisphere::Left) => "L",
                Some(Hemisphere::Right) => "R",
                Some(Hemisphere::Both) => "B",
            };
            assert_eq!(hemisphere, them.get("Hemisphere"), "{at}: Hemisphere");
            assert_eq!(
                opt(&us.domain_grandparent_id),
                them.get("DomainGrandParentID"),
                "{at}"
            );
            assert_eq!(opt(&us.originator_id), them.get("OriginatorID"), "{at}");
            assert_eq!(
                opt(&us.local_curvature_parent),
                them.get("LocalCurvatureParent"),
                "{at}: LocalCurvatureParent"
            );
            assert_eq!(
                opt(&us.local_domain_parent),
                them.get("LocalDomainParent"),
                "{at}: LocalDomainParent"
            );
            assert_eq!(opt(&us.label_dset), them.get("LabelDset"), "{at}");
            assert_eq!(opt(&us.node_marker), them.get("NodeMarker"), "{at}");
            // AFNI folds MappingRef into the parents.
            assert_eq!(them.get("MappingRef"), "", "{at}");
        }
    }
}

#[test]
fn written_spec_is_what_afni_writes() {
    for name in GOOD {
        let surfaces = resolved(name);
        let group = surfaces[0].group.clone();
        let spec = Spec::from_resolved(&group, &surfaces).unwrap();
        let theirs =
            std::fs::read_to_string(common::data(&format!("spec/{name}.rewrite.spec"))).unwrap();
        assert_eq!(spec.to_text(None, None), theirs, "{name}");
    }
}

#[test]
fn every_bad_spec_is_refused() {
    for name in BAD {
        let text = std::fs::read_to_string(common::data(&format!("spec/{name}.spec"))).unwrap();
        // Some are refused as the text is read, the rest when SUMA's checks run.
        let outcome = Spec::parse(&text).and_then(|spec| spec.resolve(format!("{name}.spec")));
        assert!(outcome.is_err(), "{name} was accepted: {outcome:?}");
    }
}

#[test]
fn what_is_refused_names_the_problem() {
    let cases = [
        ("bad_anatomical", "Anatomical"),
        ("bad_before_newsurface", "before the first NewSurface"),
        ("bad_curvature", "LocalCurvatureParent"),
        ("bad_duplicate", "twice"),
        ("bad_embed", "EmbedDimension"),
        ("bad_gibberish", "not `Key = Value`"),
        ("bad_hemisphere", "Hemisphere"),
        ("bad_mix_mapping", "MappingRef"),
        ("bad_no_type", "SurfaceType"),
        ("bad_second_no_type", "SurfaceType"),
        ("bad_state_undefined", "StateDef"),
    ];
    for (name, needle) in cases {
        let text = std::fs::read_to_string(common::data(&format!("spec/{name}.spec"))).unwrap();
        let error = Spec::parse(&text)
            .and_then(|spec| spec.resolve("x.spec"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(needle), "{name}: {error}");
    }
}

#[test]
fn a_spec_afni_wrote_is_written_back_unchanged() {
    // `ico.spec` came from AFNI's CreateIcosahedron, which uses SUMA's writer.
    let path = common::data("surface/ico.spec");
    let original = std::fs::read_to_string(&path).unwrap();
    let history = original
        .lines()
        .find_map(|l| l.strip_prefix("#History: "))
        .unwrap();
    let again = Spec::parse(&original)
        .unwrap()
        .to_text(Some("SUMA_CreateIcosahedron-main"), Some(history));
    assert_eq!(again, original);

    // So did the ones SUMA's template scripts make.
    let real = std::fs::read_to_string(common::data("real/spec/std.141.sub-3_both.spec")).unwrap();
    assert_eq!(Spec::parse(&real).unwrap().to_text(None, None), real);
}

#[test]
fn real_specs_resolve() {
    let spec = Spec::read(common::data("real/spec/std.141.sub-3_both.spec")).unwrap();
    let surfaces = spec.resolve("std.141.sub-3_both.spec").unwrap();
    assert_eq!(surfaces.len(), spec.surfaces.len());
    assert!(surfaces.iter().all(|s| s.surface_type == "GIFTI"));
    assert_eq!(surfaces[0].group, "sub-3");
    // The template writes `././SAME`, and SUMA puts the spec's `./` in front.
    assert_eq!(
        surfaces[0].local_domain_parent.as_deref(),
        Some("./././SAME")
    );
    assert_eq!(spec.states().len(), 11);
}

#[test]
fn comments_are_any_line_with_a_hash() {
    let spec = Spec::parse(
        "StateDef = a\n# a comment\nNewSurface\n SurfaceType = FreeSurfer\n SurfaceName = x.asc\n \
         Anatomical = Y # hides the whole line\n SurfaceState = a\n",
    )
    .unwrap();
    assert_eq!(spec.surfaces[0].get("Anatomical"), None);
    assert_eq!(spec.surfaces[0].get("SurfaceState"), Some("a"));
}

#[test]
fn building_a_spec_to_write() {
    let mut white = ResolvedSurface::new("FreeSurfer", "lh.white.asc", "white");
    white.anatomical = Some(true);
    white.hemisphere = Some(Hemisphere::Left);
    let mut pial = ResolvedSurface::new("FreeSurfer", "lh.pial.asc", "pial");
    pial.local_domain_parent = Some("lh.white.asc".into());
    pial.anatomical = Some(true);
    let spec = Spec::from_resolved("subj", &[white, pial]).unwrap();
    let text = spec.to_text(Some("afni-io"), Some("test"));

    assert!(text.starts_with("# afni-io generated spec file\n#History: test\n\n#define the group"));
    assert!(text.contains("\tStateDef = white\n\tStateDef = pial\n"));
    // What was written reads back, with the unset parent written as SAME.
    let back = Spec::parse(&text).unwrap();
    let surfaces = back.resolve("subj.spec").unwrap();
    assert_eq!(surfaces[0].local_domain_parent.as_deref(), Some("./SAME"));
    assert_eq!(
        surfaces[1].local_domain_parent.as_deref(),
        Some("./lh.white.asc")
    );
    assert_eq!(surfaces[1].group, "subj");
    assert_eq!(back.states(), ["white", "pial"]);

    // One group per spec, as in SUMA.
    let mut other = ResolvedSurface::new("FreeSurfer", "x.asc", "white");
    other.group = "elsewhere".into();
    assert!(Spec::from_resolved("subj", &[other]).is_err());
}

#[test]
fn write_to_disk_and_read_back() {
    let dir = std::env::temp_dir().join(format!("afni_io_spec_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.spec");
    let surface = ResolvedSurface::new("GIFTI", "lh.white.gii", "white");
    Spec::from_resolved("g", &[surface])
        .unwrap()
        .write(&path, None, None)
        .unwrap();
    let back = Spec::read(&path).unwrap().resolve(&path).unwrap();
    assert_eq!(
        back[0]
            .surface_file
            .as_deref()
            .map(|p| p.ends_with("/lh.white.gii")),
        Some(true)
    );
    std::fs::remove_dir_all(dir).unwrap();
}
