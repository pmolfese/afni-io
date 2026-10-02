//! Geometry and timing from `.HEAD` files, checked against `3dinfo` and
//! against the NIfTI sforms `3dAFNItoNIFTI` wrote from the same datasets.

mod common;

use afni_io::geometry::{dicom_to_ras, Mat44};
use afni_io::prelude::*;

/// What `3dinfo -aform_real -is_oblique -obliquity -datum -orient -nv -tr
/// -av_space` printed for one dataset (see make_volume_fixtures.sh).
struct InfoRef {
    aform_real: Mat44,
    is_oblique: bool,
    obliquity: f64,
    orient: String,
    nv: usize,
    tr: f64,
    view: Option<String>,
}

fn info_ref(name: &str) -> InfoRef {
    let text = std::fs::read_to_string(common::data(&format!("volume/{name}.aform.txt"))).unwrap();
    let mut lines = text.lines().filter(|l| !l.starts_with('#'));
    let mut aform_real = afni_io::geometry::identity();
    for row in aform_real.iter_mut().take(3) {
        let values: Vec<f64> = lines
            .next()
            .unwrap()
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        row.copy_from_slice(&values);
    }
    // e.g. "\t1\t10.000\tfloat\tRAI\t1\t0.000000\t+orig" (no view for NIfTI)
    let fields: Vec<&str> = lines
        .next()
        .unwrap()
        .split('\t')
        .filter(|f| !f.is_empty())
        .collect();
    InfoRef {
        aform_real,
        is_oblique: fields[0] == "1",
        obliquity: fields[1].parse().unwrap(),
        orient: fields[3].to_string(),
        nv: fields[4].parse().unwrap(),
        tr: fields[5].parse().unwrap(),
        view: fields.get(6).map(|v| v.to_string()),
    }
}

fn assert_mat_close(actual: &Mat44, expected: &Mat44, tolerance: f64, what: &str) {
    for r in 0..4 {
        for c in 0..4 {
            assert!(
                (actual[r][c] - expected[r][c]).abs() <= tolerance,
                "{what}: [{r}][{c}] is {} but expected {}\n  actual   {actual:?}\n  expected {expected:?}",
                actual[r][c],
                expected[r][c]
            );
        }
    }
}

fn heads() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(common::data("volume"))
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.strip_suffix(".HEAD").map(str::to_string)
        })
        .collect();
    names.sort();
    assert!(names.len() >= 14, "{names:?}");
    names
}

#[test]
fn every_head_matches_3dinfo() {
    for name in heads() {
        let head = Header::read(common::data(&format!("volume/{name}.HEAD"))).unwrap();
        let info = info_ref(&name);
        // 3dinfo prints 6 decimals.
        assert_mat_close(&head.ijk_to_dicom().unwrap(), &info.aform_real, 5e-6, &name);
        assert_eq!(head.is_oblique(), info.is_oblique, "{name}: is_oblique");
        assert!(
            (head.obliquity() - info.obliquity).abs() < 5e-4,
            "{name}: obliquity"
        );
        assert_eq!(
            head.orientation_string().unwrap(),
            info.orient,
            "{name}: orient"
        );
        assert_eq!(head.nvals(), info.nv, "{name}: nv");
        let tr = head.time_axis().and_then(|t| t.tr_seconds()).unwrap_or(0.0);
        assert!(
            (tr - info.tr).abs() < 5e-7,
            "{name}: TR {tr} vs {}",
            info.tr
        );
        assert_eq!(
            head.view().map(|v| v.suffix().to_string()),
            info.view,
            "{name}: view"
        );
    }
}

#[test]
fn slice_timing_matches_3dinfo() {
    for name in heads() {
        let head = Header::read(common::data(&format!("volume/{name}.HEAD"))).unwrap();
        let text =
            std::fs::read_to_string(common::data(&format!("volume/{name}.slice_timing.txt")))
                .unwrap();
        let expected: Vec<f64> = text.trim().split('|').map(|v| v.parse().unwrap()).collect();
        let offsets = head
            .time_axis()
            .map(|t| t.slice_offsets)
            .unwrap_or_default();
        if offsets.is_empty() {
            // 3dinfo prints one zero per slice when there is no slice timing.
            assert!(expected.iter().all(|&t| t == 0.0), "{name}: {expected:?}");
        } else {
            assert_eq!(offsets, expected, "{name}");
        }
    }
    let axis = Header::read(common::data("volume/slicetimed+orig.HEAD"))
        .unwrap()
        .time_axis()
        .unwrap();
    assert_eq!((axis.nt, axis.step, axis.slice_offsets.len()), (3, 2.5, 6));
}

#[test]
fn cardinal_matrix_equals_real_matrix_unless_oblique() {
    for name in heads() {
        let head = Header::read(common::data(&format!("volume/{name}.HEAD"))).unwrap();
        let cardinal = head.ijk_to_dicom_cardinal().unwrap();
        if head.is_oblique() {
            // The 10-degree rotation in make_volume_fixtures.sh only replaced
            // IJK_TO_DICOM_REAL; the cardinal grid is the plain f32 one.
            assert_eq!(name, "oblique+orig");
            assert_eq!(cardinal[0][..3], [1.0, 0.0, 0.0], "{name}");
        } else {
            assert_mat_close(&cardinal, &head.ijk_to_dicom().unwrap(), 1e-6, &name);
        }
    }
}

#[test]
fn head_ijk_to_ras_equals_nifti_sform() {
    for (nii, source) in [
        ("s16.nii.gz", "s16+orig"),
        ("stat.nii", "stat+orig"),
        ("stat_pure.nii", "stat+orig"),
        ("lpi.nii", "lpi+orig"),
        ("oblique.nii", "oblique+orig"),
    ] {
        let head = Header::read(common::data(&format!("volume/{source}.HEAD"))).unwrap();
        let nifti = Nifti::read(common::data(&format!("volume/{nii}"))).unwrap();
        assert_mat_close(
            &head.ijk_to_ras().unwrap(),
            &nifti.header.affine(),
            1e-5,
            nii,
        );
        // And 3dinfo reads the NIfTI back to the same RAI matrix.
        assert_mat_close(
            &dicom_to_ras(&nifti.header.affine()),
            &info_ref(nii).aform_real,
            5e-6,
            nii,
        );
    }
}
