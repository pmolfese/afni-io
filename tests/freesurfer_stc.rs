//! FreeSurfer binary surfaces and MNE `.stc` files.
//!
//! The bytes in these tests are spelled out by hand from the format
//! descriptions in `src/freesurfer.rs` and `src/stc.rs`, not made by the
//! writers, so a reader and writer that agree with each other but not with the
//! format cannot pass. For FreeSurfer surfaces, AFNI's own reader is also run
//! on what we write when `AFNI_IO_LIVE=1` (`ConvertSurface -i_fs`). AFNI has no
//! `.stc` reader.

mod common;

use afni_io::freesurfer::FreeSurferSurface;
use afni_io::niml::NimlData;
use afni_io::stc::Stc;
use afni_io::surface::Surface;
use afni_io::Error;

// ---- FreeSurfer ---------------------------------------------------------------

/// A one-triangle surface, byte by byte.
fn triangle_bytes(footer: &[u8]) -> Vec<u8> {
    let mut b = vec![0xFF, 0xFF, 0xFE]; // magic
    b.extend_from_slice(b"created by test\n");
    b.extend_from_slice(b"a comment\n");
    b.extend_from_slice(&[0, 0, 0, 3]); // 3 vertices
    b.extend_from_slice(&[0, 0, 0, 1]); // 1 triangle
    for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, -2.5]] {
        for x in v {
            b.extend_from_slice(&x.to_be_bytes());
        }
    }
    b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2]); // 0 1 2
    b.extend_from_slice(footer);
    b
}

#[test]
fn reads_a_triangle_surface() {
    let surface = FreeSurferSurface::parse(&triangle_bytes(b"")).unwrap();
    assert_eq!(surface.created_by, "created by test");
    assert_eq!(surface.comment, "a comment");
    assert_eq!(
        surface.vertices,
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, -2.5]]
    );
    assert_eq!(surface.faces, [[0, 1, 2]]);
    assert!(surface.footer.is_empty());
    assert!(FreeSurferSurface::is_triangle_surface(&triangle_bytes(b"")));
}

#[test]
fn writes_the_same_bytes() {
    for footer in [&b""[..], b"\x00\x00\x00\x14some footer"] {
        let bytes = triangle_bytes(footer);
        let surface = FreeSurferSurface::parse(&bytes).unwrap();
        assert_eq!(surface.to_bytes().unwrap(), bytes);
    }
}

#[test]
fn the_footer_is_kept_and_searchable() {
    // The volume geometry recent FreeSurfer versions append: a tag, then lines.
    let footer = b"\x00\x00\x00\x14valid = 1  # volume info valid\nfilename = orig.mgz\n\
                   volume = 256 256 256\nvoxelsize = 1.000 1.000 1.000\n\
                   cras   = 5.25 -10.5 0.125\n";
    let surface = FreeSurferSurface::parse(&triangle_bytes(footer)).unwrap();
    assert_eq!(surface.footer, footer);
    assert_eq!(
        surface.footer_values("cras"),
        Some(vec![5.25, -10.5, 0.125])
    );
    assert_eq!(
        surface.footer_values("volume"),
        Some(vec![256.0, 256.0, 256.0])
    );
    assert_eq!(surface.footer_values("valid"), Some(vec![1.0]));
    assert_eq!(surface.footer_values("filename"), None);
    assert_eq!(surface.footer_values("xras"), None);
}

#[test]
fn refuses_what_it_cannot_read() {
    let good = triangle_bytes(b"");
    let parse = |bytes: &[u8]| FreeSurferSurface::parse(bytes);

    // Quads (both magic numbers) are unsupported, as in AFNI.
    for magic in [[0xFF, 0xFF, 0xFF], [0xFF, 0xFF, 0xFD]] {
        let mut bytes = good.clone();
        bytes[..3].copy_from_slice(&magic);
        assert!(matches!(parse(&bytes), Err(Error::Unsupported(_))));
    }
    // Something else entirely.
    assert!(matches!(parse(b"#!ascii version"), Err(Error::Parse(_))));
    assert!(parse(b"").is_err());
    // Cut short at every point.
    for end in 0..good.len() {
        assert!(parse(&good[..end]).is_err(), "cut at {end}");
    }
    // A triangle naming a missing vertex, or a negative one.
    let n = good.len();
    for index in [3i32, -1] {
        let mut bytes = good.clone();
        bytes[n - 4..].copy_from_slice(&index.to_be_bytes());
        assert!(matches!(parse(&bytes), Err(Error::Invalid(_))), "{index}");
    }
    // A count past AFNI's limit, and a negative count.
    let counts_at = 3 + "created by test\n".len() + "a comment\n".len();
    for count in [2_000_001i32, -1] {
        let mut bytes = good.clone();
        bytes[counts_at..counts_at + 4].copy_from_slice(&count.to_be_bytes());
        assert!(matches!(parse(&bytes), Err(Error::Invalid(_))), "{count}");
    }
    // A NaN coordinate.
    let mut bytes = good.clone();
    let first_vertex = counts_at + 8;
    bytes[first_vertex..first_vertex + 4].copy_from_slice(&f32::NAN.to_be_bytes());
    assert!(matches!(parse(&bytes), Err(Error::Invalid(_))));
    // No newline where one is due.
    let mut bytes = vec![0xFF, 0xFF, 0xFE];
    bytes.extend(std::iter::repeat(b'x').take(6000));
    assert!(parse(&bytes).is_err());
}

#[test]
fn refuses_to_write_what_afni_would_not_read() {
    let mut surface = FreeSurferSurface::new(vec![[0.0; 3]; 3], vec![[0, 1, 3]]);
    assert!(surface.to_bytes().is_err()); // vertex 3 does not exist
    surface.faces = vec![[0, 1, 2]];
    assert!(surface.to_bytes().is_ok());
    surface.comment = "two\nlines".into();
    assert!(surface.to_bytes().is_err());
    surface.comment.clear();
    surface.created_by = "x".repeat(5000);
    assert!(surface.to_bytes().is_err());
}

#[test]
fn converts_to_and_from_an_ascii_surface() {
    let ascii = Surface::read(common::data("surface/ico.asc")).unwrap();
    let binary = FreeSurferSurface::from_surface(&ascii);
    assert_eq!(binary.vertices.len(), 42);
    assert_eq!(binary.faces.len(), 80);
    let back = FreeSurferSurface::parse(&binary.to_bytes().unwrap()).unwrap();
    assert_eq!(back, binary);
    assert_eq!(back.to_surface().vertices, ascii.vertices);
    assert_eq!(back.to_surface().faces, ascii.faces);
}

#[test]
fn afni_reads_the_surface_we_write() {
    if std::env::var_os("AFNI_IO_LIVE").is_none() {
        eprintln!("skipping: set AFNI_IO_LIVE=1 to hand our surface to ConvertSurface");
        return;
    }
    let dir = std::env::temp_dir().join(format!("afni_io_fs_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ascii = Surface::read(common::data("surface/ico.asc")).unwrap();
    FreeSurferSurface::from_surface(&ascii)
        .write(dir.join("ico.white"))
        .unwrap();

    let status = std::process::Command::new("ConvertSurface")
        .current_dir(&dir)
        .env("AFNI_DONT_LOGFILE", "YES")
        .args(["-i_fs", "ico.white", "-o_fs", "back", "-overwrite"])
        .output()
        .expect("ConvertSurface not found (is AFNI on PATH?)");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );

    let back = Surface::read(dir.join("back.asc")).unwrap();
    assert_eq!(back.faces, ascii.faces);
    assert_eq!(back.vertices.len(), ascii.vertices.len());
    for (a, b) in back.vertices.iter().zip(&ascii.vertices) {
        for k in 0..3 {
            // AFNI prints six decimals.
            assert!((a[k] - b[k]).abs() < 1e-5, "{a:?} {b:?}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- MNE .stc -------------------------------------------------------------------

/// Two vertices at three times, byte by byte. Times are in milliseconds.
fn stc_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(-100.0f32).to_be_bytes()); // tmin, ms
    b.extend_from_slice(&(50.0f32).to_be_bytes()); // tstep, ms
    b.extend_from_slice(&2u32.to_be_bytes()); // vertices
    b.extend_from_slice(&7u32.to_be_bytes());
    b.extend_from_slice(&9u32.to_be_bytes());
    b.extend_from_slice(&3u32.to_be_bytes()); // times
                                              // Time-major: both vertices at t0, then both at t1, then t2.
    for v in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b
}

#[test]
fn reads_a_source_estimate() {
    let stc = Stc::parse(&stc_bytes()).unwrap();
    assert_eq!(stc.tmin, -0.1);
    assert_eq!(stc.tstep, 0.05);
    assert_eq!(stc.vertices, [7, 9]);
    assert_eq!(
        stc.time_points,
        [vec![1.0, 2.0], vec![3.0, 4.0], vec![5.0, 6.0]]
    );
    assert!((stc.time(2) - 0.0).abs() < 1e-9);
    assert_eq!(stc.to_bytes().unwrap(), stc_bytes());
}

#[test]
fn refuses_a_damaged_estimate() {
    let good = stc_bytes();
    for end in 0..good.len() {
        assert!(Stc::parse(&good[..end]).is_err(), "cut at {end}");
    }
    let mut longer = good.clone();
    longer.extend_from_slice(&[0, 0, 0, 0]);
    assert!(Stc::parse(&longer).is_err());

    let patch = |at: usize, bytes: [u8; 4]| {
        let mut b = good.clone();
        b[at..at + 4].copy_from_slice(&bytes);
        Stc::parse(&b)
    };
    assert!(patch(4, 0.0f32.to_be_bytes()).is_err()); // step must be positive
    assert!(patch(4, f32::NAN.to_be_bytes()).is_err());
    assert!(patch(0, f32::INFINITY.to_be_bytes()).is_err());
    assert!(patch(8, 0u32.to_be_bytes()).is_err()); // no vertices
                                                    // A header that promises far more than the file holds is refused without
                                                    // allocating for it.
    assert!(patch(8, u32::MAX.to_be_bytes()).is_err());
    assert!(patch(20, u32::MAX.to_be_bytes()).is_err());
    assert!(patch(20, 0u32.to_be_bytes()).is_err()); // no times
}

#[test]
fn refuses_to_write_a_ragged_estimate() {
    let mut stc = Stc::parse(&stc_bytes()).unwrap();
    stc.time_points[1].pop();
    assert!(stc.to_bytes().is_err());
    let mut stc = Stc::parse(&stc_bytes()).unwrap();
    stc.tstep = 0.0;
    assert!(stc.to_bytes().is_err());
    stc.tstep = 0.05;
    stc.vertices.clear();
    assert!(stc.to_bytes().is_err());
}

#[test]
fn becomes_a_surface_dataset() {
    let stc = Stc::parse(&stc_bytes()).unwrap();
    let dset = stc.to_dataset().unwrap();
    assert_eq!(dset.node_indices.as_deref(), Some(&[7u32, 9][..]));
    assert_eq!(dset.rows(), 2);
    assert_eq!(dset.column_count(), 3);
    assert_eq!(dset.time_step, Some(0.05));
    assert_eq!(dset.node_for_row(1), Some(9));
    assert_eq!(dset.data.get(1, 2), Some(6.0)); // vertex 9 at the third time
    assert_eq!(
        dset.column_labels(),
        ["t=-0.100000", "t=-0.050000", "t=0.000000"]
    );

    // It writes and reads back as a NIML dataset.
    let element = dset.to_element().unwrap();
    assert!(matches!(element.data, NimlData::Group(_)));
    let wire = afni_io::niml::serialize_binary(&[element]);
    let again =
        afni_io::dset::NimlDataset::from_element(&afni_io::niml::parse(&wire).unwrap()[0]).unwrap();
    assert_eq!(again.node_indices, dset.node_indices);
    assert_eq!(again.data, dset.data);
    assert_eq!(again.time_step, dset.time_step);
}
