//! Correlation metadata: AFNI's `Correl(samples, nfit, nort)` versus the NIfTI
//! standard's one-parameter form, and the p-values each implies through
//! `afni-core`. Fixtures: `volume/correl*` (see `make_volume_fixtures.sh`).

// These tests exercise the deprecated lenient `DataArray::stat`, to prove it keeps its
// old behavior while callers migrate to `stat_with_origin`.
#![allow(deprecated)]

mod common;

use afni_core::stat::{IntentOrigin, StatKind, StatSpec};
use afni_core::stats::{p_value, Tail};
use afni_io::gifti::{DataArray, Encoding, IndexOrder};
use afni_io::prelude::*;

fn stats_of(name: &str) -> Vec<Option<StatSpec>> {
    read_any(common::data(&format!("volume/{name}")))
        .unwrap()
        .stats()
        .unwrap()
}

#[test]
fn afni_correlation_sub_brick_keeps_its_three_parameters() {
    // BRICK_STATSYM `Correl(30,2,1)` in the .HEAD.
    let spec = stats_of("correl+orig.HEAD")[0].clone().unwrap();
    assert_eq!(
        (spec.kind, spec.params.as_slice()),
        (StatKind::Correl, &[30.0, 2.0, 1.0][..])
    );
    // The same file as NIfTI with the AFNI extension: identical.
    assert_eq!(stats_of("correl.nii")[0].as_ref(), Some(&spec));
}

#[test]
fn afni_written_nifti_header_is_recognised_as_afni_parameters() {
    // `3dAFNItoNIFTI -pure` copies (30, 2, 1) into intent_p1..3. The structure
    // (nfit >= 1) identifies it as AFNI's form, so the answer matches the .HEAD.
    let pure = stats_of("correl_pure.nii")[0].clone().unwrap();
    assert_eq!(pure.params, [30.0, 2.0, 1.0]);
}

#[test]
fn standards_compliant_one_parameter_intent_becomes_the_equivalent_afni_form() {
    // intent_p1 = 18 (dof), p2 = p3 = 0.
    let std = stats_of("correl_standard.nii")[0].clone().unwrap();
    assert_eq!(std.params, [19.0, 1.0, 0.0]); // samples = dof + 1, nfit = 1, nort = 0
                                              // Zero-filling would have produced Correl(18,0,0), which is invalid.
    assert!(p_value(&std, 0.4, Tail::TwoSided).is_ok());
}

#[test]
fn the_two_conventions_give_different_p_values_for_the_same_numbers() {
    // This is why provenance matters: p1 = 18 means dof 18 in the NIfTI standard
    // but "18 samples" to AFNI. Same r, different probability.
    let r = 0.4;
    let standard = StatSpec::from_intent(2, [18.0, 0.0, 0.0], IntentOrigin::Standard)
        .unwrap()
        .unwrap();
    let afni = StatSpec::from_intent(2, [18.0, 1.0, 0.0], IntentOrigin::Afni)
        .unwrap()
        .unwrap();
    let p_std = p_value(&standard, r, Tail::TwoSided).unwrap().p();
    let p_afni = p_value(&afni, r, Tail::TwoSided).unwrap().p();
    assert!(p_std < p_afni, "dof 18 vs dof 17: {p_std} vs {p_afni}");
}

fn gifti_array(p: [&str; 3]) -> DataArray {
    DataArray {
        intent: 2,
        dims: vec![4],
        index_order: IndexOrder::RowMajor,
        encoding: Encoding::Ascii,
        coordsys: vec![],
        meta: vec![
            ("intent_p1".into(), p[0].into()),
            ("intent_p2".into(), p[1].into()),
            ("intent_p3".into(), p[2].into()),
        ],
        data: TypedArray::Float32(vec![0.1, 0.2, 0.3, 0.4]),
    }
}

#[test]
fn gifti_correlation_intent_follows_the_same_rules() {
    // AFNI-written (nfit >= 1) and standard (p2 = p3 = 0) are both understood.
    let afni = gifti_array(["30", "2", "1"]).stat().unwrap();
    assert_eq!(afni.params, [30.0, 2.0, 1.0]);
    let std = gifti_array(["18", "0", "0"]).stat().unwrap();
    assert_eq!(std.params, [19.0, 1.0, 0.0]);
    // An impossible mixture is not guessed at: no statistic is reported.
    assert!(gifti_array(["18", "0.5", "0"]).stat().is_none());
}

// ---------------------------------------------------------------------------
// Malformed correlation metadata must be loud, and correct files must be right.
// ---------------------------------------------------------------------------

use afni_io::adapt::{
    gifti_to_core, gifti_to_core_with, volume_to_core, volume_to_core_with, AdaptOptions,
    MetadataPolicy,
};

/// `correl_pure.nii` with `intent_p2` patched to `p2`, giving a header that
/// neither AFNI (nfit >= 1) nor the NIfTI standard (p2 = 0) would write.
fn malformed_correlation_volume(p2: f32) -> Volume {
    let mut bytes = std::fs::read(common::data("volume/correl_pure.nii")).unwrap();
    bytes[60..64].copy_from_slice(&p2.to_le_bytes()); // intent_p2
    Volume::Nifti(Box::new(Nifti::from_bytes(&bytes).unwrap()))
}

#[test]
fn well_formed_files_of_both_conventions_come_out_right_through_the_adapters() {
    // The AFNI-style header and the standard one-parameter header both become
    // usable columns with the statistic attached, no configuration needed.
    let afni = volume_to_core(&read_any(common::data("volume/correl_pure.nii")).unwrap()).unwrap();
    assert_eq!(afni.columns()[0].stat().unwrap().params, [30.0, 2.0, 1.0]);
    let std =
        volume_to_core(&read_any(common::data("volume/correl_standard.nii")).unwrap()).unwrap();
    let spec = std.columns()[0].stat().unwrap();
    assert_eq!(spec.params, [19.0, 1.0, 0.0]);
    // ...and the p-values reflect each convention's own degrees of freedom.
    // (Upper tail: Correl(30,2,1) has nfit = 2, a multiple correlation, which is
    // one-sided by definition.)
    let p = |ds: &afni_core::dataset::Dataset| {
        p_value(ds.columns()[0].stat().unwrap(), 0.4, Tail::Upper)
            .unwrap()
            .p()
    };
    assert!(p(&afni) > 0.0 && p(&std) > 0.0 && (p(&afni) - p(&std)).abs() > 1e-6);
}

#[test]
fn a_malformed_correlation_is_an_error_not_a_missing_statistic() {
    let vol = malformed_correlation_volume(0.5);
    assert!(vol.stats().is_err());
    assert!(vol.stats_with_origin(IntentOrigin::Standard).is_err());
    let err = volume_to_core(&vol).unwrap_err().to_string();
    assert!(err.contains("correlation"), "{err}");
}

#[test]
fn a_malformed_correlation_can_be_skipped_with_a_warning() {
    let opts = AdaptOptions {
        statistics: MetadataPolicy::SkipWithWarning,
        ..AdaptOptions::default()
    };
    let (ds, warnings) = volume_to_core_with(&malformed_correlation_volume(0.5), &opts).unwrap();
    assert!(
        ds.columns()[0].stat().is_none(),
        "the data loads without the statistic"
    );
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].what, "statistic");
    assert!(warnings[0].message.contains("correlation"));
}

#[test]
fn stating_the_writer_overrides_detection_and_a_wrong_claim_is_an_error() {
    // Claiming "standard" for AFNI's (30, 2, 1) is refused: p2 and p3 must be 0.
    let afni_file = read_any(common::data("volume/correl_pure.nii")).unwrap();
    assert!(afni_file.stats_with_origin(IntentOrigin::Standard).is_err());
    let opts = AdaptOptions {
        intent_origin: IntentOrigin::Standard,
        ..AdaptOptions::default()
    };
    assert!(volume_to_core_with(&afni_file, &opts).is_err());
    // Claiming "AFNI" for the one-parameter file reads the numbers verbatim, as
    // asked. That is Correl(18, 0, 0): invalid for AFNI (nfit >= 1), so a p-value
    // from it is an error rather than a wrong number.
    let std_file = read_any(common::data("volume/correl_standard.nii")).unwrap();
    let stats = std_file.stats_with_origin(IntentOrigin::Afni).unwrap();
    let spec = stats[0].clone().unwrap();
    assert_eq!(spec.params, [18.0, 0.0, 0.0]);
    assert!(p_value(&spec, 0.4, Tail::TwoSided).is_err());
}

#[test]
fn gifti_stat_problems_are_reported_by_the_checked_api() {
    // Non-numeric parameter text: the old Option API says "no statistic"; the
    // checked API says why.
    let bad = gifti_array(["thirty", "2", "1"]);
    assert!(bad.stat().is_none());
    assert!(bad.stat_with_origin(IntentOrigin::Unknown).is_err());
    // Malformed correlation: checked API errors, and honours an override.
    let mixed = gifti_array(["18", "0.5", "0"]);
    assert!(mixed.stat_with_origin(IntentOrigin::Unknown).is_err());
    assert!(mixed.stat_with_origin(IntentOrigin::Standard).is_err());
    // Missing parameters count as zero (the NIfTI default): the standard form.
    let mut missing = gifti_array(["18", "0", "0"]);
    missing.meta.retain(|(k, _)| k == "intent_p1");
    assert_eq!(
        missing
            .stat_with_origin(IntentOrigin::Unknown)
            .unwrap()
            .unwrap()
            .params,
        [19.0, 1.0, 0.0]
    );
}

#[test]
fn gifti_adapter_surfaces_or_skips_a_bad_statistic() {
    let mut gii = Gifti::read(common::data("surface/dense_asc.gii.dset")).unwrap();
    // Turn the first data array into a malformed correlation.
    let a = gii
        .data_arrays
        .iter_mut()
        .find(|a| a.dims.len() == 1 || a.dims.get(1) == Some(&1))
        .unwrap();
    a.intent = 2;
    a.meta.retain(|(k, _)| !k.starts_with("intent_p"));
    a.meta.push(("intent_p1".into(), "18".into()));
    a.meta.push(("intent_p2".into(), "0.5".into()));
    assert!(gifti_to_core(&gii, Some(42)).is_err());
    let lenient = AdaptOptions {
        statistics: MetadataPolicy::SkipWithWarning,
        ..AdaptOptions::default()
    };
    let (ds, warnings) = gifti_to_core_with(&gii, Some(42), &lenient).unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(ds.columns()[warnings[0].column].stat().is_none());
}
