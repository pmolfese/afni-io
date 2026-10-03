//! Statistic metadata and the NIfTI AFNI extension, against files written by
//! AFNI (`3drefit -substatpar`, `3dAFNItoNIFTI`, `ConvertDset`).

// These tests exercise the deprecated lenient `DataArray::stat`, to prove it keeps its
// old behavior while callers migrate to `stat_with_origin`.
#![allow(deprecated)]

mod common;

use afni_io::head::AttributeValue;
use afni_io::prelude::*;

fn stat_head() -> Header {
    Header::read(common::data("volume/stat+orig.HEAD")).unwrap()
}

fn statsyms(stats: &[Option<StatSpec>]) -> Vec<String> {
    stats
        .iter()
        .map(|s| s.as_ref().map_or("none".into(), StatSpec::to_statsym))
        .collect()
}

#[test]
fn head_stats_from_statsym_and_from_stataux_agree() {
    // make_volume_fixtures.sh: 3drefit -substatpar 0 fitt 23 -substatpar 1 fift 2 40
    let head = stat_head();
    assert_eq!(head.string("BRICK_STATSYM"), Some("Ttest(23);Ftest(2,40)"));
    assert_eq!(statsyms(&head.brick_stats()), ["Ttest(23)", "Ftest(2,40)"]);

    // Without BRICK_STATSYM, AFNI falls back to the BRICK_STATAUX records.
    let mut aux_only = head.clone();
    aux_only.attributes.retain(|a| a.name != "BRICK_STATSYM");
    assert_eq!(
        aux_only.floats("BRICK_STATAUX").unwrap(),
        [0.0, 3.0, 1.0, 23.0, 1.0, 4.0, 2.0, 2.0, 40.0]
    );
    assert_eq!(aux_only.brick_stats(), head.brick_stats());

    // Datasets without statistics.
    let plain = Header::read(common::data("volume/s16+orig.HEAD")).unwrap();
    assert_eq!(plain.brick_stats(), vec![None; 3]);
}

#[test]
fn nifti_afni_extension_carries_the_whole_head() {
    let head = stat_head();
    let nii = Nifti::read(common::data("volume/stat.nii")).unwrap();
    let ext = nii.afni_header().unwrap().expect("AFNI extension");

    // Every attribute matches the .HEAD it was converted from, except the
    // ID code, which 3dAFNItoNIFTI regenerates.
    assert_eq!(
        ext.attributes.iter().map(|a| &a.name).collect::<Vec<_>>(),
        head.attributes.iter().map(|a| &a.name).collect::<Vec<_>>()
    );
    for attr in &ext.attributes {
        if attr.name != "IDCODE_STRING" {
            assert_eq!(Some(&attr.value), head.get(&attr.name), "{}", attr.name);
        }
    }
    assert!(ext.idcode().unwrap().starts_with("AFN_"));

    // So the typed accessors work the same on a .nii as on a .HEAD.
    assert_eq!(ext.brick_labels(), ["Tstat#0", "Fstat*with*tildes"]);
    assert_eq!(statsyms(&ext.brick_stats()), ["Ttest(23)", "Ftest(2,40)"]);
    assert_eq!(ext.ijk_to_ras().unwrap(), nii.header.affine());

    // -pure writes no extension, and a multi-brick NIfTI has no intent code.
    let pure = Nifti::read(common::data("volume/stat_pure.nii")).unwrap();
    assert!(pure.extensions.is_empty());
    assert_eq!(pure.afni_header().unwrap(), None);
    assert_eq!(pure.header.intent_code, 0);
}

#[test]
fn nifti_afni_extension_round_trips_through_write() {
    let nii = Nifti::read(common::data("volume/stat.nii")).unwrap();
    let ext = nii.afni_header().unwrap().unwrap();
    let back = Nifti::from_bytes(&nii.to_bytes()).unwrap();
    assert_eq!(back.data, nii.data);
    assert_eq!(back.afni_header().unwrap().unwrap(), ext);

    // Replacing the header: the change survives, the rest is untouched.
    let mut edited = nii.clone();
    let mut header = ext.clone();
    header.set(
        "BRICK_STATSYM",
        AttributeValue::String("Zscore();none\0".into()),
    );
    edited.set_afni_header(&header);
    assert_eq!(edited.extensions.len(), 1);
    let reread = Nifti::from_bytes(&edited.to_bytes()).unwrap();
    let stats = reread.afni_header().unwrap().unwrap().brick_stats();
    assert_eq!(statsyms(&stats), ["Zscore()", "none"]);
}

#[test]
fn niml_dset_statsym_uses_the_same_type() {
    let dset = NimlDataset::read(common::data("surface/stat.niml.dset")).unwrap();
    let stats = dset.column_stats();
    assert_eq!(statsyms(&stats), ["Ttest(10)", "Ftest(2,30)"]);
}

#[test]
fn gifti_intents_use_the_same_type() {
    // ConvertDset -o_gii_asc of stat.niml.dset (make_surface_fixtures.sh).
    let gii = Gifti::read(common::data("surface/stat.gii.dset")).unwrap();
    let stats: Vec<_> = gii.data_arrays.iter().map(|da| da.stat()).collect();
    assert_eq!(statsyms(&stats), ["Ttest(10)", "Ftest(2,30)"]);

    // The same statistics, from the NIML version of the dataset.
    let dset = NimlDataset::read(common::data("surface/stat.niml.dset")).unwrap();
    let from_niml = dset.column_stats();
    assert_eq!(stats, from_niml);

    // Surfaces carry no statistic.
    let surf = Gifti::read(common::data("surface/ico_ascii.gii")).unwrap();
    assert!(surf.data_arrays.iter().all(|da| da.stat().is_none()));
}

#[test]
fn reference_isc_gifti_stats() {
    let Some(path) = common::reference("ISC_lh_theta_neg.gii.dset") else {
        return;
    };
    let gii = Gifti::read(path).unwrap();
    let stats: Vec<_> = gii.data_arrays.iter().filter_map(|da| da.stat()).collect();
    assert!(!stats.is_empty());
    eprintln!(
        "ISC stats: {:?}",
        statsyms(&stats.into_iter().map(Some).collect::<Vec<_>>())
    );
}
