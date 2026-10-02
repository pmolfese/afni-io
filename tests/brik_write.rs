//! Writing AFNI datasets: read -> write -> read must give the same voxels and
//! the same header, apart from what `Brik::write` is documented to refresh.

mod common;

use std::path::PathBuf;

use afni_io::head::AttributeValue;
use afni_io::prelude::*;

/// A fresh scratch directory per test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("afni_io_write_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Attributes `Brik::write` replaces: a new ID, little-endian output, and
/// recomputed statistics.
const REFRESHED: [&str; 4] = [
    "IDCODE_STRING",
    "IDCODE_DATE",
    "BYTEORDER_STRING",
    "BRICK_STATS",
];

fn fixture_heads() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(common::data("volume"))
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.strip_suffix(".HEAD").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn every_fixture_round_trips_plain_and_gzipped() {
    let dir = scratch("roundtrip");
    for name in fixture_heads() {
        let original = Brik::read(common::data(&format!("volume/{name}.HEAD"))).unwrap();
        for suffix in [".BRIK", ".BRIK.gz"] {
            let target = dir.join(format!("{name}{suffix}"));
            let paths = original.write(&target).unwrap();
            assert_eq!(paths.brik.as_ref(), Some(&target), "{name}");

            let back = Brik::read(&paths.head).unwrap();
            assert_eq!(back.dimensions, original.dimensions, "{name}");
            assert_eq!(
                back.sub_bricks, original.sub_bricks,
                "{name}{suffix}: voxels"
            );

            for attr in &original.header.attributes {
                if REFRESHED.contains(&attr.name.as_str()) {
                    continue;
                }
                assert_eq!(
                    back.header.get(&attr.name),
                    Some(&attr.value),
                    "{name}: {}",
                    attr.name
                );
            }
            assert_eq!(back.header.string("BYTEORDER_STRING"), Some("LSB_FIRST"));
            assert_ne!(
                back.header.idcode(),
                original.header.idcode(),
                "{name}: new ID"
            );

            // Our BRICK_STATS agree with the ones AFNI wrote (AFNI prints
            // about 7 significant digits).
            let ours = back.header.floats("BRICK_STATS").unwrap();
            let afni = original.header.floats("BRICK_STATS").unwrap();
            assert_eq!(ours.len(), afni.len(), "{name}");
            for (a, b) in ours.iter().zip(&afni) {
                assert!(
                    (a - b).abs() <= 1e-5 * b.abs().max(1.0),
                    "{name}: stats {ours:?} vs {afni:?}"
                );
            }
            std::fs::remove_file(&paths.head).unwrap();
            std::fs::remove_file(paths.brik.unwrap()).unwrap();
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn new_dataset_has_the_requested_grid() {
    let dir = scratch("new");
    let (nx, ny, nz) = (3, 2, 2);
    let values: Vec<f32> = (0..nx * ny * nz).map(|v| v as f32 * 0.5).collect();
    let mask: Vec<u8> = (0..nx * ny * nz).map(|v| (v % 2) as u8).collect();
    let brik = Brik::new(
        [nx, ny, nz],
        [Orientation::L2R, Orientation::P2A, Orientation::I2S],
        [10.0, -20.0, 5.0],
        [-2.0, -2.0, 3.0],
        vec![
            SubBrick {
                data: BrickData::Float(values.clone()),
                factor: 0.0,
            },
            SubBrick {
                data: BrickData::Byte(mask),
                factor: 0.0,
            },
        ],
    )
    .unwrap();

    // A bare prefix takes the header's view.
    let paths = brik.write(dir.join("made")).unwrap();
    assert_eq!(paths.head, dir.join("made+orig.HEAD"));
    let back = Brik::read(&paths.head).unwrap();
    assert_eq!(back.header.orientation_string().as_deref(), Some("LPI"));
    assert_eq!(
        back.header.ijk_to_dicom().unwrap(),
        [
            [-2.0, 0.0, 0.0, 10.0],
            [0.0, -2.0, 0.0, -20.0],
            [0.0, 0.0, 3.0, 5.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    );
    assert!(!back.header.is_oblique());
    assert_eq!(back.sub_brick(0).unwrap().to_f32().unwrap(), values);
    assert_eq!(back.value(1, 0, 0, 1), Some(1.0));
    assert_eq!(back.header.brick_types(), [3, 0]);
    assert_eq!(
        back.header.floats("BRICK_STATS").unwrap(),
        [0.0, 5.5, 0.0, 1.0]
    );

    // Naming another view sets SCENE_DATA to match.
    let paths = brik.write(dir.join("made+tlrc.HEAD")).unwrap();
    assert_eq!(Header::read(&paths.head).unwrap().view(), Some(View::Tlrc));

    // IDCODE_DATE looks like ctime output.
    let date = back.header.string("IDCODE_DATE").unwrap().to_string();
    assert_eq!(date.len(), 24, "{date:?}");
    assert_eq!(&date[13..14], ":", "{date:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn refuses_incomplete_or_ambiguous_writes() {
    let dir = scratch("refuse");
    let source = common::data("volume/s16+orig.HEAD");

    let partial = Brik::read_sub_bricks(&source, &[1]).unwrap();
    let err = partial
        .write(dir.join("partial+orig"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("sub-brick 0 is not loaded"), "{err}");

    let mut wrong = Brik::read(&source).unwrap();
    wrong.sub_bricks[2] = Some(SubBrick {
        data: BrickData::Short(vec![1]),
        factor: 0.0,
    });
    assert!(wrong.write(dir.join("wrong+orig")).is_err());

    // A .BRIK.gz next to the target would shadow or be shadowed.
    let full = Brik::read(&source).unwrap();
    full.write(dir.join("both+orig.BRIK.gz")).unwrap();
    let err = full.write(dir.join("both+orig")).unwrap_err().to_string();
    assert!(err.contains("both+orig.BRIK.gz exists"), "{err}");

    // A string attribute set in code gets its terminating NUL on disk.
    let mut labelled = full.clone();
    labelled
        .header
        .set("BRICK_LABS", AttributeValue::String("a b~c\0d".into()));
    let paths = labelled.write(dir.join("labelled+orig")).unwrap();
    let text = std::fs::read_to_string(&paths.head).unwrap();
    assert!(text.contains("count = 8\n'a b*c~d~"), "{text}");
    assert_eq!(
        Header::read(&paths.head).unwrap().brick_labels(),
        ["a b*c", "d", "#2"]
    );
    std::fs::remove_dir_all(&dir).ok();
}
