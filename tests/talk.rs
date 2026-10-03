//! The AFNI ⇄ SUMA talk protocol: port numbering checked against `afni
//! -list_ports`, framing, and the surface, crosshair and colour elements
//! checked against messages AFNI itself sent.
//!
//! Run with `cargo test --features talk`.
#![cfg(feature = "talk")]

mod common;

use afni_io::niml::{self, NimlData};
use afni_io::surface::Surface;
use afni_io::talk::*;
use afni_io::Error;

// ---- ports ----------------------------------------------------------------

/// What `afni -list_ports` printed, as `(name, port)` pairs.
fn afni_ports(case: &str) -> Vec<(String, u16)> {
    let text = std::fs::read_to_string(common::data(&format!("talk/ports_{case}.txt"))).unwrap();
    text.lines()
        .map(|line| {
            // "9: AFNI_PLUGOUT_TCP_0 has port 7955"
            let mut words = line.split_whitespace();
            let _index = words.next().unwrap();
            let name = words.next().unwrap().to_string();
            let port = words.last().unwrap().parse().unwrap();
            (name, port)
        })
        .collect()
}

/// The table afni-io builds for the same environment. As AFNI does, a bad
/// offset or bloc in the environment means the default ports.
fn our_ports(env: &[(&str, &str)]) -> (Vec<(String, u16)>, Option<Error>) {
    let get = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    };
    let (offset, error) = match offset_from_env(get) {
        Ok(offset) => (offset, None),
        Err(error) => (None, Some(error)),
    };
    let ports = Ports::with_env(offset, &PortEnv::from_lookup(get)).unwrap();
    let table = ports.iter().map(|(n, p)| (n.to_string(), p)).collect();
    (table, error)
}

#[test]
fn port_table_matches_afni_list_ports() {
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);
    let cases: &[Case] = &[
        ("default", &[], false),
        ("np3000", &[("AFNI_PORT_OFFSET", "3000")], false),
        ("npb3", &[("AFNI_PORT_BLOC", "3")], false),
        ("npb_max", &[("AFNI_PORT_BLOC", "2686")], false),
        ("np_max", &[("AFNI_PORT_OFFSET", "65500")], false),
        (
            "env_ports",
            &[
                ("SUMA_AFNI_TCP_PORT", "4000"),
                ("SUMA_AFNI_TCP_PORT2", "4001"),
                ("SUMA_MATLAB_LISTEN_PORT", "4005"),
                ("AFNI_PLUGOUT_TCP_BASE", "7000"),
            ],
            false,
        ),
        // With an offset, only the plugout base still moves its ports.
        (
            "np_ignores",
            &[
                ("AFNI_PORT_OFFSET", "3000"),
                ("SUMA_AFNI_TCP_PORT", "4000"),
                ("AFNI_PLUGOUT_TCP_BASE", "7000"),
            ],
            false,
        ),
        // Both set: the bloc wins.
        (
            "bloc_wins",
            &[("AFNI_PORT_BLOC", "3"), ("AFNI_PORT_OFFSET", "3000")],
            false,
        ),
        // Below 1024 is skipped quietly; out of range is an error that AFNI
        // reports and then uses the defaults for.
        ("np_too_small", &[("AFNI_PORT_OFFSET", "1023")], false),
        ("np_too_big", &[("AFNI_PORT_OFFSET", "65501")], true),
        ("npb_too_big", &[("AFNI_PORT_BLOC", "2687")], true),
        // AFNI_NIML_FIRST_PORT has no effect (init_ports_list overwrites it).
        ("first_port", &[("AFNI_NIML_FIRST_PORT", "4000")], false),
    ];
    for (case, env, is_error) in cases {
        let (ours, error) = our_ports(env);
        assert_eq!(error.is_some(), *is_error, "{case}: {error:?}");
        assert_eq!(ours, afni_ports(case), "{case}");
    }
}

#[test]
fn offsets_and_blocs() {
    assert_eq!(offset_from_bloc(0).unwrap(), 1024);
    assert_eq!(offset_from_bloc(3).unwrap(), 1096);
    assert_eq!(offset_from_bloc(MAX_PORT_BLOC).unwrap(), 65488);
    assert!(offset_from_bloc(MAX_PORT_BLOC + 1).is_err());
    assert_eq!(bloc_from_offset(1096), Some(3));
    assert_eq!(bloc_from_offset(1023), None);
    assert!(check_offset(1023).is_err());
    assert!(check_offset(MAX_PORT_OFFSET + 1).is_err());
    // A bloc and its offset give the same table.
    assert_eq!(
        Ports::new(Some(offset_from_bloc(3).unwrap()))
            .unwrap()
            .bloc(),
        Some(3)
    );
}

#[test]
fn ports_by_name() {
    let ports = Ports::new(None).unwrap();
    assert_eq!(ports.get(AFNI_SUMA_NIML), Some(53211));
    assert_eq!(ports.afni_suma_niml(), 53211);
    assert_eq!(ports.drivesuma(), 53219);
    assert_eq!(ports.get("PLUGOUT_TT_PORT"), Some(8001));
    assert_eq!(ports.get("NO_SUCH_PORT"), None);
    assert_eq!(ports.iter().count(), PORT_NAMES.len());
    assert!(Ports::new(Some(10)).is_err());
    // A plugout base near the top of the range would run past 65535.
    let env = PortEnv {
        afni_plugout_tcp_base: Some(65535),
        ..PortEnv::default()
    };
    assert!(Ports::with_env(None, &env).is_err());
}

// ---- framing ---------------------------------------------------------------

type Mesh = (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[u32; 3]>);

fn icosahedron() -> Mesh {
    let surface = Surface::read(common::data("surface/ico.asc")).unwrap();
    // Unit normals pointing away from the centre are enough for a test.
    let normals = surface
        .vertices
        .iter()
        .map(|[x, y, z]| {
            let n = (x * x + y * y + z * z).sqrt().max(1e-6);
            [x / n, y / n, z / n]
        })
        .collect();
    (surface.vertices, normals, surface.faces)
}

fn info() -> SurfaceInfo {
    SurfaceInfo {
        surface_idcode: "surface-test".into(),
        surface_label: "lh.smoothwm".into(),
        local_domain_parent_id: "lh.smoothwm".into(),
        local_domain_parent: "lh.smoothwm".into(),
        specfile_name: Some("test.spec".into()),
        specfile_path: Some("/data/SUMA".into()),
        volume_idcode: Some("XYZ_volume".into()),
        volume_headname: Some("SurfVol+orig.HEAD".into()),
        volume_filecode: Some("/data/SUMA/SurfVol".into()),
        volume_dirname: Some("/data/SUMA".into()),
        controls: Some(SurfaceControls::for_label("lh.smoothwm")),
    }
}

#[test]
fn registration_is_framed_and_ordered() {
    let (vertices, normals, triangles) = icosahedron();
    let geometry = Geometry {
        vertices: &vertices,
        normals: &normals,
        triangles: &triangles,
    };
    let bytes = registration_bytes(&info(), &geometry).unwrap();
    assert!(bytes.starts_with(KEEP_READING));
    assert!(bytes.ends_with(PAUSE_READING));

    let mut reader = MessageReader::new();
    let elements = reader.push(&bytes).unwrap();
    assert_eq!(reader.pending(), 0);
    let names: Vec<_> = elements.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["SUMA_ixyz", "SUMA_node_normals", "SUMA_ijk"]);

    let n = vertices.len();
    let types = ["int,3*float", "3*float", "3*int"];
    let rows = [n, n, triangles.len()];
    for ((element, ty), rows) in elements.iter().zip(types).zip(rows) {
        let NimlData::Numeric(m) = &element.data else {
            panic!("{} is not numeric", element.name)
        };
        assert_eq!(niml::ni_type_string(&m.column_types()), ty);
        assert_eq!(m.rows, rows);
        for (key, value) in info().attributes() {
            assert_eq!(element.attrs.get(&key), Some(&value), "{key}");
        }
    }
    let NimlData::Numeric(ixyz) = &elements[0].data else {
        unreachable!()
    };
    for (i, v) in vertices.iter().enumerate() {
        assert_eq!(ixyz.get(i, 0), Some(i as f64));
        for (c, value) in v.iter().enumerate() {
            assert_eq!(ixyz.get(i, c + 1), Some(f64::from(*value)));
        }
    }
    let NimlData::Numeric(ijk) = &elements[2].data else {
        unreachable!()
    };
    for (i, t) in triangles.iter().enumerate() {
        for (c, index) in t.iter().enumerate() {
            assert_eq!(ijk.get(i, c), Some(f64::from(*index)));
        }
    }
}

#[test]
fn registration_controls_and_optional_attributes() {
    let mut bare = info();
    bare.controls = None;
    bare.specfile_name = None;
    bare.volume_idcode = None;
    let attrs = bare.attributes();
    assert!(!attrs.contains_key("afni_surface_controls_lines"));
    assert!(!attrs.contains_key("surface_specfile_name"));
    assert!(!attrs.contains_key("volume_idcode"));
    assert_eq!(attrs["surface_idcode"], "surface-test");

    let with = info().attributes();
    assert_eq!(with["afni_surface_controls_toggle"], "on");
    assert_eq!(with["afni_surface_controls_lines"], "#00ff00");
    assert_eq!(SurfaceControls::for_label("rh.smoothwm").lines, "#ffff00");
    assert_eq!(SurfaceControls::for_label("lh.pial").lines, "#0000ff");
    assert_eq!(SurfaceControls::for_label("rh.pial").lines, "#ff0000");
    assert_eq!(SurfaceControls::for_label("inflated").lines, "#ff69b4");
}

#[test]
fn bad_geometry_is_refused() {
    let v = [[0.0; 3]; 3];
    let t = [[0, 1, 2]];
    let ok = Geometry {
        vertices: &v,
        normals: &v,
        triangles: &t,
    };
    assert!(surface_elements(&info(), &ok).is_ok());
    let few_normals = Geometry {
        normals: &v[..2],
        ..ok
    };
    assert!(surface_elements(&info(), &few_normals).is_err());
    let past_end = [[0, 1, 3]];
    let bad = Geometry {
        triangles: &past_end,
        ..ok
    };
    assert!(surface_elements(&info(), &bad).is_err());
}

#[test]
fn reader_gives_the_same_elements_however_the_bytes_arrive() {
    let (vertices, normals, triangles) = icosahedron();
    let geometry = Geometry {
        vertices: &vertices,
        normals: &normals,
        triangles: &triangles,
    };
    let mut stream = registration_bytes(&info(), &geometry).unwrap();
    stream.extend(encode(&[drive_afni_element("SWITCH_UNDERLAY A.x")]));
    stream.extend(Crosshair::default().to_element().pipe_encode());
    let whole = MessageReader::new().push(&stream).unwrap();
    assert_eq!(whole.len(), 5);

    for chunk in [1, 2, 7, 64, 1000] {
        let mut reader = MessageReader::new();
        let mut got = Vec::new();
        for piece in stream.chunks(chunk) {
            got.extend(reader.push(piece).unwrap());
        }
        assert_eq!(reader.pending(), 0, "chunk {chunk}");
        assert_eq!(got, whole, "chunk {chunk}");
    }
}

trait PipeEncode {
    fn pipe_encode(self) -> Vec<u8>;
}
impl PipeEncode for niml::NimlElement {
    fn pipe_encode(self) -> Vec<u8> {
        encode(&[self])
    }
}

#[test]
fn reader_caps_what_it_holds_and_discards_on_error() {
    let mut reader = MessageReader::with_max_pending(64);
    // An element that never closes looks like one still arriving.
    assert!(reader.push(b"<Never closes>").unwrap().is_empty());
    let error = reader.push(&[b'x'; 100]).unwrap_err();
    assert!(error.to_string().contains("incomplete"), "{error}");
    assert_eq!(reader.pending(), 0);

    let mut reader = MessageReader::new();
    assert!(reader
        .push(br#"<x ni_form="binary" ni_dimen="1" >"#)
        .is_err());
    assert_eq!(reader.pending(), 0);
    // It recovers once the caller resynchronises.
    assert_eq!(
        reader
            .push(b"<ok ni_form=\"ni_group\" ></ok>")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reader_drops_processing_instructions() {
    let mut reader = MessageReader::new();
    let mut bytes = KEEP_READING.to_vec();
    bytes.extend(encode(&[drive_afni_element("QUIT")]));
    bytes.extend_from_slice(PAUSE_READING);
    let elements = reader.push(&bytes).unwrap();
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].name, "ni_do");
    assert_eq!(reader.pending(), 0);
}

#[test]
fn drive_afni_forms() {
    let element = drive_afni_element("OPEN_WINDOW A.axialimage");
    let text = String::from_utf8(encode(std::slice::from_ref(&element))).unwrap();
    assert!(text.contains("ni_verb=\"DRIVE_AFNI\""), "{text}");
    assert!(
        text.contains("ni_object=\"OPEN_WINDOW A.axialimage\""),
        "{text}"
    );
    assert_eq!(niml::parse_str(&text).unwrap()[0].attrs, element.attrs);

    assert_eq!(
        drive_afni_procins("SET_DICOM_XYZ 1 2 3").unwrap(),
        b"<?drive_afni cmd='SET_DICOM_XYZ 1 2 3' ?>\n"
    );
    assert!(drive_afni_procins("it's").is_err());

    let mut i = info();
    assert_eq!(
        switch_underlay_command(&i).as_deref(),
        Some("SWITCH_UNDERLAY A.XYZ_volume")
    );
    i.volume_idcode = Some(String::new());
    assert_eq!(
        switch_underlay_command(&i).as_deref(),
        Some("SWITCH_UNDERLAY A.SurfVol+orig.HEAD")
    );
    i.volume_headname = None;
    assert_eq!(
        switch_underlay_command(&i).as_deref(),
        Some("SWITCH_UNDERLAY A.SurfVol")
    );
    i.volume_filecode = None;
    assert_eq!(switch_underlay_command(&i), None);
}

// ---- messages AFNI sent ------------------------------------------------------

fn fixture_elements(name: &str) -> Vec<niml::NimlElement> {
    niml::parse(&std::fs::read(common::data(&format!("talk/{name}"))).unwrap()).unwrap()
}

#[test]
fn afni_crosshair_group_is_read() {
    let elements = fixture_elements("afni_crosshair.niml");
    assert_eq!(elements.len(), 1);
    let message = CrosshairMessage::from_element(&elements[0]).unwrap();

    assert_eq!(
        message.crosshair.position,
        [-39.15466, -24.8533, -0.5454788]
    );
    assert_eq!(
        message.crosshair.surface_idcode.as_deref(),
        Some("surface-514eae48587dbc08")
    );
    assert_eq!(message.crosshair.node_index, Some(79484));
    let underlay = message.underlay.as_ref().unwrap();
    assert_eq!(
        underlay.idcode.as_deref(),
        Some("XYZ_EpNaQDmaOtcizseN1TiEVw")
    );
    assert_eq!(underlay.voxel_ijk, Some([85, 123, 150]));
    assert_eq!(underlay.has_time_axis, Some(false));
    assert_eq!(underlay.values, [74.0]);
    assert_eq!(message.v2s_node_values.as_deref(), Some(&[48.0][..]));

    // Writing it back gives a group that reads the same.
    let again = niml::parse(&encode(&[message.to_element()])).unwrap();
    assert_eq!(CrosshairMessage::from_element(&again[0]).unwrap(), message);
}

#[test]
fn afni_irgba_is_read_and_written_back_identically() {
    let elements = fixture_elements("afni_irgba_10.niml");
    let irgba = Irgba::from_element(&elements[0]).unwrap();
    assert_eq!(irgba.surface_idcode, "surface-e201563c46d0ec54");
    assert_eq!(irgba.local_domain_parent_id.as_deref(), Some("lh.smoothwm"));
    assert_eq!(
        irgba.volume_idcode.as_deref(),
        Some("XYZ_EpNaQDmaOtcizseN1TiEVw")
    );
    assert_eq!(
        irgba.function_idcode.as_deref(),
        Some("XYZ_EpNaQDmaOtcizseN1TiEVw")
    );
    assert_eq!(irgba.threshold.as_deref(), Some("0.0001"));
    assert_eq!(irgba.nodes, (0..10).collect::<Vec<u32>>());
    assert_eq!(irgba.rgba[0], [255, 54, 0, 255]);
    assert_eq!(irgba.rgba[1], [255, 74, 0, 255]);

    // The element we write parses to the same element AFNI sent: the same
    // attributes, `ni_type` and values.
    let ours = niml::parse(&encode(&[irgba.to_element().unwrap()])).unwrap();
    assert_eq!(ours, elements);

    // And the body is AFNI's bytes, byte for byte.
    let afni = std::fs::read(common::data("talk/afni_irgba_10.niml")).unwrap();
    let body = |bytes: &[u8]| {
        let start = bytes.iter().position(|b| *b == b'>').unwrap() + 1;
        let end = bytes
            .windows(13)
            .position(|w| w == b"</SUMA_irgba>")
            .unwrap();
        bytes[start..end].to_vec()
    };
    assert_eq!(body(&encode(&[irgba.to_element().unwrap()])), body(&afni));
}

#[test]
fn empty_irgba_means_no_overlay() {
    let irgba = Irgba {
        surface_idcode: "surface-test".into(),
        ..Irgba::default()
    };
    let wire = encode(&[irgba.to_element().unwrap()]);
    let back = Irgba::from_element(&niml::parse(&wire).unwrap()[0]).unwrap();
    assert!(back.nodes.is_empty() && back.rgba.is_empty());
    assert_eq!(back, irgba);

    // A zero-row element, with or without a declared type, reads as empty too.
    let declared = br#"<SUMA_irgba ni_type="int,4*byte" ni_dimen="0" surface_idcode="s" />"#;
    let element = niml::parse(declared).unwrap().remove(0);
    assert!(Irgba::from_element(&element).unwrap().nodes.is_empty());
}

#[test]
fn irgba_errors() {
    let mut irgba = Irgba {
        surface_idcode: "s".into(),
        nodes: vec![1, 2],
        rgba: vec![[0; 4]],
        ..Irgba::default()
    };
    assert!(irgba.to_element().is_err());
    irgba.rgba.push([1, 2, 3, 4]);
    let element = irgba.to_element().unwrap();
    let mut no_id = element.clone();
    no_id.attrs.remove("surface_idcode");
    assert!(Irgba::from_element(&no_id).is_err());
    let mut wrong = element;
    wrong.name = "other".into();
    assert!(Irgba::from_element(&wrong).is_err());
}

// ---- crosshair, built here ----------------------------------------------------

#[test]
fn crosshair_to_afni() {
    let crosshair = Crosshair::on_surface(&info(), 65921, [6.5, -9.25, -0.5]);
    let wire = encode(&[crosshair.to_element()]);
    let text = String::from_utf8_lossy(&wire);
    assert!(text.starts_with("<SUMA_crosshair_xyz"), "{text}");
    assert!(
        text.contains("ni_type=\"float\"") && text.contains("ni_dimen=\"3\""),
        "{text}"
    );

    let back = Crosshair::from_element(&niml::parse(&wire).unwrap()[0]).unwrap();
    assert_eq!(back, crosshair);
    assert_eq!(back.node_index, Some(65921));
    assert!(!back.icor);

    let seed = Crosshair {
        icor: true,
        ..crosshair
    };
    let element = seed.to_element();
    assert!(element.attrs.contains_key("Do_icor"));
    assert!(Crosshair::from_element(&element).unwrap().icor);
}

#[test]
fn crosshair_reader_follows_suma() {
    let parse = |text: &str| niml::parse_str(text).unwrap().remove(0);
    // Exactly three floats in one vector, as SUMA_crosshair_xyz requires.
    let four =
        parse(r#"<SUMA_crosshair_xyz ni_type="float" ni_dimen="4">1 2 3 4</SUMA_crosshair_xyz>"#);
    assert!(Crosshair::from_element(&four).is_err());
    let ints =
        parse(r#"<SUMA_crosshair_xyz ni_type="int" ni_dimen="3">1 2 3</SUMA_crosshair_xyz>"#);
    assert!(Crosshair::from_element(&ints).is_err());
    // An empty or negative node id means no node.
    for node in ["", "-1"] {
        let text = format!(
            r#"<SUMA_crosshair_xyz ni_type="float" ni_dimen="3" surface_nodeid="{node}">1 2 3</SUMA_crosshair_xyz>"#
        );
        let c = Crosshair::from_element(&parse(&text)).unwrap();
        assert_eq!(c.node_index, None, "{node:?}");
        assert_eq!(c.position, [1.0, 2.0, 3.0]);
    }
    let other = parse(r#"<other ni_type="float" ni_dimen="3">1 2 3</other>"#);
    assert!(Crosshair::from_element(&other).is_err());
}

#[test]
fn flip_xy_is_ras_to_rai() {
    assert_eq!(flip_xy([1.0, 2.0, 3.0]), [-1.0, -2.0, 3.0]);
    assert_eq!(flip_xy(flip_xy([1.0, -2.0, 3.0])), [1.0, -2.0, 3.0]);
}
