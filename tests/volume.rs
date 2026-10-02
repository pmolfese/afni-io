//! `read_any`: the same data read from AFNI and from NIfTI must look the same.

mod common;

use afni_io::prelude::*;

#[test]
fn afni_and_nifti_versions_agree() {
    for (nii, head) in [
        ("s16.nii.gz", "s16+orig"),
        ("stat.nii", "stat+orig"),
        ("lpi.nii", "lpi+orig"),
        ("oblique.nii", "oblique+orig"),
    ] {
        let a = read_any(common::data(&format!("volume/{head}.HEAD"))).unwrap();
        let n = read_any(common::data(&format!("volume/{nii}"))).unwrap();
        assert!(a.as_afni().is_some() && n.as_nifti().is_some());
        assert_eq!(a.dimensions(), n.dimensions(), "{nii}");
        assert_eq!(a.nvols(), n.nvols(), "{nii}");
        let (ra, rn) = (a.ijk_to_ras().unwrap(), n.ijk_to_ras().unwrap());
        for r in 0..4 {
            for c in 0..4 {
                assert!(
                    (ra[r][c] - rn[r][c]).abs() < 1e-5,
                    "{nii}: {ra:?} vs {rn:?}"
                );
            }
        }
        for t in 0..a.nvols() {
            assert_eq!(a.frame_f32(t), n.frame_f32(t), "{nii} volume {t}");
        }
        assert_eq!(a.value(3, 4, 5, 0), n.value(3, 4, 5, 0));
        assert_eq!(a.frame_f32(a.nvols()), None);
        // Labels and statistics travel in the NIfTI's AFNI extension.
        assert_eq!(a.labels().unwrap(), n.labels().unwrap(), "{nii}");
        assert_eq!(a.stats().unwrap(), n.stats().unwrap(), "{nii}");
    }

    let stat = read_any(common::data("volume/stat.nii")).unwrap();
    let names: Vec<_> = stat
        .stats()
        .unwrap()
        .iter()
        .map(|s| s.as_ref().unwrap().to_statsym())
        .collect();
    assert_eq!(names, ["Ttest(23)", "Ftest(2,40)"]);

    // Without the extension, AFNI's default labels and no statistics.
    let pure = read_any(common::data("volume/stat_pure.nii")).unwrap();
    assert_eq!(pure.labels().unwrap(), ["#0", "#1"]);
    assert_eq!(pure.stats().unwrap(), [None, None]);
}

#[test]
fn reading_selected_afni_volumes() {
    let v = read_any_volumes(common::data("volume/s16gz+orig.BRIK.gz"), &[2]).unwrap();
    assert_eq!(v.nvols(), 3);
    assert_eq!(v.frame_f32(0), None);
    // make_volume_fixtures.sh: v = i + 10j + 100k + 1000t - 300.
    assert_eq!(
        v.value(1, 2, 3, 2),
        Some((1 + 20 + 300 + 2000 - 300) as f32)
    );

    let rgb = read_any(common::data("volume/rgb+orig.HEAD")).unwrap();
    assert_eq!(rgb.frame_f32(0), None, "RGB has no scalar frame");
}

#[test]
fn nifti_header_intent_applies_to_every_volume() {
    let mut nifti = Nifti::read(common::data("volume/stat_pure.nii")).unwrap();
    nifti.header.intent_code = 3; // NIFTI_INTENT_TTEST
    nifti.header.intent_p1 = 12.0;
    let dir = std::env::temp_dir().join(format!("afni_io_volume_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ttest.nii");
    nifti.write(&path).unwrap();

    let stats = read_any(&path).unwrap().stats().unwrap();
    assert_eq!(stats.len(), 2);
    for s in stats {
        assert_eq!(s.unwrap().to_statsym(), "Ttest(12)");
    }
    std::fs::remove_dir_all(&dir).ok();
}
