//! A real NIML surface time series through afni-core's seed correlation: file to
//! core dataset (`adapt`), clean and correlate (`afni_core::instacorr`), and the
//! result back as a dataset carrying a correlation statistic.
//!
//! The fixture `surface/ts_groups.niml.dset` (made by `make_instacorr_fixture.sh`)
//! has 42 nodes and 40 time points at TR 2 s: nodes 0..21 follow one signal, nodes
//! 21..42 another. The TR comes from the file, not from the options.

mod common;

use afni_core::instacorr::{prepare_dataset, InstaCorrOptions};
use afni_core::stat::StatKind;
use afni_io::adapt::niml_to_core;
use afni_io::prelude::*;

#[test]
fn seed_correlation_on_a_real_time_series_file() {
    let raw = NimlDataset::read(common::data("surface/ts_groups.niml.dset")).unwrap();
    assert_eq!(raw.time_step, Some(2.0));
    // The file lists node indices (a sparse dataset), so the surface size is supplied.
    let env = niml_to_core(&raw, Some(42)).unwrap();
    let ds = &env.core;
    assert_eq!(ds.row_count(), 42);

    let options = InstaCorrOptions {
        tr_seconds: None, // from the dataset
        band: Some((0.02, 0.2)),
        polort: 1,
        ..InstaCorrOptions::default()
    };
    let prepared = prepare_dataset(ds, &options).unwrap();
    assert_eq!((prepared.row_count(), prepared.sample_count()), (42, 40));

    // Seed on node 5: its own group correlates strongly, the other weakly.
    let c = prepared.correlate_row(5).unwrap();
    assert!((c.values[5] - 1.0).abs() < 1e-5);
    let same: f32 = (0..21)
        .filter(|&n| n != 5)
        .map(|n| c.values[n].abs())
        .sum::<f32>()
        / 20.0;
    let other: f32 = (21..42).map(|n| c.values[n].abs()).sum::<f32>() / 21.0;
    assert!(same > 0.8, "same-group mean |r| {same}");
    assert!(other < same - 0.3, "other-group {other} vs same {same}");

    // The statistic: Correl(40 samples, 1 fit, removed dimensions).
    let stat = c.stat.clone().expect("a correlation statistic");
    assert_eq!(stat.kind, StatKind::Correl);
    assert_eq!(stat.params[0], 40.0);
    assert_eq!(stat.params[2], prepared.removed_dof() as f64);
    // Degrees of freedom were really reduced, so the p-value is larger than a naive one.
    let dof = c.degrees_of_freedom().unwrap();
    assert!(dof < 38, "dof {dof}");

    // The result as a dataset on the same surface rows, spec attached.
    let out = c.to_dataset(ds, "seed 5").unwrap();
    assert_eq!(out.row_count(), 42);
    assert_eq!(out.columns()[0].stat(), Some(&stat));

    // An ROI seed (the whole first group) does at least as well on that group.
    let roi = prepared
        .correlate_rows(&(0..21).collect::<Vec<_>>())
        .unwrap();
    assert!(roi.values[..21].iter().all(|&v| v > 0.8));
}
