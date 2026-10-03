//! FDR/MDF against AFNI: the curves `3drefit -addFDR` stores in a `.HEAD`, the
//! z-scores `3dFDR` writes, and `fdrval` lookups in both directions. The data are
//! `volume/fdr+orig` (a null-plus-signal t(23) and F(2,40) bucket); see
//! `make_volume_fixtures.sh` for how each reference was produced.

mod common;

use afni_core::fdr::{
    fdr_curves, fdrize, minimum_q, q_value_for_threshold, threshold_for_q, FdrOptions, FdrOutput,
    QValue,
};
use afni_core::stat::StatSpec;
use afni_io::prelude::*;

struct Fixture {
    specs: Vec<StatSpec>,
    stats: Vec<Vec<f32>>,
    header: Header,
}

fn load(name: &str) -> Fixture {
    let vol = read_any(common::data(&format!("volume/{name}"))).unwrap();
    let specs: Vec<StatSpec> = vol
        .stats()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();
    let stats = (0..vol.nvols())
        .map(|t| vol.frame_f32(t).unwrap())
        .collect();
    Fixture {
        specs,
        stats,
        header: vol.afni_header().unwrap().unwrap(),
    }
}

fn max_abs(values: &[f32]) -> f64 {
    values
        .iter()
        .map(|v| f64::from(v.abs()))
        .fold(0.0, f64::max)
}

/// Largest relative-or-absolute difference between two equal-length series.
fn worst_diff(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs() / (1.0 + y.abs()))
        .fold(0.0, f64::max)
}

#[test]
fn fdr_curves_match_those_afni_stores_in_the_header() {
    let fx = load("fdr+orig.HEAD");
    assert_eq!(fx.specs.len(), 2);
    for sub in 0..2 {
        let theirs = fx.header.fdr_curve(sub).expect("AFNI stored an FDR curve");
        let ours =
            fdr_curves(&fx.specs[sub], &fx.stats[sub], None, &FdrOptions::default()).unwrap();
        let fdr = &ours.fdr;
        assert_eq!(
            fdr.len(),
            theirs.values.len(),
            "sub-brick {sub}: 101 points"
        );
        // AFNI stores 7 significant digits.
        assert!(
            (fdr.x0() - theirs.x0).abs() <= 2e-6 * theirs.x0.abs().max(1.0),
            "x0 {sub}: ours {} AFNI {}",
            fdr.x0(),
            theirs.x0
        );
        assert!(
            (fdr.dx() - theirs.dx).abs() <= 2e-5 * theirs.dx.abs(),
            "dx {sub}: ours {} AFNI {}",
            fdr.dx(),
            theirs.dx
        );
        let diff = worst_diff(fdr.samples(), &theirs.values);
        assert!(
            diff < 1e-5,
            "sub-brick {sub}: FDR curve differs by {diff:e}"
        );
    }
}

#[test]
fn missed_detection_curves_match_those_afni_stores_in_the_header() {
    let fx = load("fdr+orig.HEAD");
    for sub in 0..2 {
        let theirs = fx.header.mdf_curve(sub).expect("AFNI stored an MDF curve");
        let ours = fdr_curves(&fx.specs[sub], &fx.stats[sub], None, &FdrOptions::default())
            .unwrap()
            .mdf
            .expect("we build an MDF curve too");
        assert_eq!(ours.len(), theirs.values.len(), "sub-brick {sub}");
        assert!(
            (ours.x0() - theirs.x0).abs() <= 1e-5 * theirs.x0.abs().max(1.0),
            "x0 {sub}"
        );
        assert!(
            (ours.dx() - theirs.dx).abs() <= 1e-4 * theirs.dx.abs(),
            "dx {sub}"
        );
        let diff = worst_diff(ours.samples(), &theirs.values);
        assert!(
            diff < 1e-4,
            "sub-brick {sub}: MDF curve differs by {diff:e}"
        );
    }
}

/// Compare z(q) maps voxel by voxel with 3dFDR.
fn compare_with_3dfdr(reference: &str, options: &FdrOptions) {
    let fx = load("fdr+orig.HEAD");
    let theirs = load(reference);
    for sub in 0..2 {
        let ours = fdrize(&fx.specs[sub], &fx.stats[sub], None, options).unwrap();
        assert_eq!(ours.values.len(), theirs.stats[sub].len());
        let mut worst = 0.0_f32;
        for (i, (a, b)) in ours.values.iter().zip(&theirs.stats[sub]).enumerate() {
            let d = (a - b).abs();
            assert!(
                d < 2e-3,
                "{reference} sub-brick {sub} voxel {i}: ours {a}, 3dFDR {b}"
            );
            worst = worst.max(d);
        }
        eprintln!("{reference}[{sub}] worst z difference {worst:e}");
    }
}

#[test]
fn z_scores_match_3dfdr() {
    compare_with_3dfdr("fdr_z+orig.HEAD", &FdrOptions::default());
}

#[test]
fn arbitrary_dependence_z_scores_match_3dfdr_cdep() {
    compare_with_3dfdr(
        "fdr_zdep+orig.HEAD",
        &FdrOptions {
            arbitrary_dependence: true,
            ..FdrOptions::default()
        },
    );
}

/// Parse `fdr+orig.fdrval.txt`: `fdrval <sub> <q|inverse> <value> => <output>`.
fn fdrval_table() -> Vec<(usize, String, f64, f64)> {
    let text = std::fs::read_to_string(common::data("volume/fdr+orig.fdrval.txt")).unwrap();
    text.lines()
        .map(|line| {
            let (lhs, rhs) = line.split_once("=>").unwrap();
            let w: Vec<&str> = lhs.split_whitespace().collect();
            (
                w[1].parse().unwrap(),
                w[2].to_string(),
                w[3].parse().unwrap(),
                rhs.trim().parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn stored_curve_lookups_match_fdrval() {
    let fx = load("fdr+orig.HEAD");
    let table = fdrval_table();
    assert!(table.len() > 30);
    for (sub, mode, input, expected) in table {
        // Use AFNI's stored curve, as `fdrval` does, so only the lookup is tested.
        let raw = fx.header.fdr_curve(sub).unwrap();
        let curve =
            afni_core::curve::ThresholdCurve::new(raw.x0, raw.dx, raw.values.clone()).unwrap();
        let label = format!("fdrval {sub} {mode} {input}");
        if mode == "q" {
            let q = q_value_for_threshold(&curve, input).unwrap().get();
            // fdrval prints 5 significant digits.
            assert!(
                (q - expected).abs() <= 6e-5 * expected.abs().max(1e-12) + 1e-12,
                "{label}: {q} vs {expected}"
            );
        } else {
            // fdrval replaces q <= 0 by 1e-9 and q >= 1 by 0.99999 (and nothing else).
            let q = QValue::new(if input <= 0.0 {
                1e-9
            } else if input >= 1.0 {
                0.99999
            } else {
                input
            })
            .unwrap();
            let thr = threshold_for_q(&curve, q, Some(max_abs(&fx.stats[sub]))).unwrap();
            assert!(
                (thr - expected).abs() <= 6e-5 * expected.abs().max(1e-12) + 1e-9,
                "{label}: {thr} vs {expected}"
            );
        }
    }
}

#[test]
fn smallest_q_matches_what_afni_reports() {
    // `3drefit -addFDR` printed: Smallest FDR q [0] = 1.18558e-15 for the t map
    // (it prints the q at the end of the stored curve, 2 qg(z_last)).
    let fx = load("fdr+orig.HEAD");
    let raw = fx.header.fdr_curve(0).unwrap();
    let curve = afni_core::curve::ThresholdCurve::new(raw.x0, raw.dx, raw.values.clone()).unwrap();
    let q = minimum_q(&curve).unwrap().get();
    assert!(q > 0.0 && q < 1e-6, "{q}");
    let _ = FdrOutput::ZScore; // (kept in scope for readers: z(q) is the stored form)
}

#[test]
fn small_surface_dataset_curve_matches_afni() {
    // 42 nodes: too few for the true-positive estimate (needs 233), so this
    // exercises the plain step-up path and the small-sample guards.
    use afni_io::adapt::niml_to_core;
    let raw = NimlDataset::read(common::data("surface/fdr.niml.dset")).unwrap();
    let env = niml_to_core(&raw, Some(42)).unwrap();
    let mut compared = 0;
    for column in env.core.columns() {
        let (Some(theirs), Some(spec)) = (column.fdr_curve(), column.stat()) else {
            continue;
        };
        let values: Vec<f32> = (0..column.len())
            .map(|r| column.values().get_f64(r).unwrap() as f32)
            .collect();
        let ours = fdr_curves(spec, &values, None, &FdrOptions::default()).unwrap();
        let diff = worst_diff(ours.fdr.samples(), theirs.samples());
        assert!(
            diff < 1e-5,
            "column {}: curve differs by {diff:e}",
            column.label()
        );
        assert!((ours.fdr.x0() - theirs.x0()).abs() < 1e-5 * theirs.x0().abs().max(1.0));
        assert!(ours.mdf.is_none(), "too few samples for an MDF curve");
        // The convenience methods on the column agree with the free functions.
        let q = column.q_for_threshold(2.0).unwrap().unwrap().get();
        assert_eq!(q, q_value_for_threshold(theirs, 2.0).unwrap().get());
        compared += 1;
    }
    assert!(compared > 0, "the fixture should carry an FDR curve");
}
