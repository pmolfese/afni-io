//! Cross-validation: a `.gii.dset` (GIfTI) and the paired `.niml.dset` for the
//! same dataset must carry identical numeric data; only the container differs.

mod common;

use std::path::Path;

fn assert_gii_matches_niml(gii_path: &Path, niml_path: &Path) {
    let gii = afni_io::gifti::Gifti::read(gii_path).expect("failed to read .gii.dset");
    let dset = afni_io::dset::NimlDataset::read(niml_path).expect("failed to read .niml.dset");

    let ncols = dset.column_count();
    let nrows = dset.rows();

    // GIfTI stores the node index as its own NODE_INDEX array; skip it.
    let columns: Vec<_> = gii
        .data_arrays
        .iter()
        .filter(|da| da.intent != afni_io::gifti::intent::NODE_INDEX)
        .collect();
    assert_eq!(
        columns.len(),
        ncols,
        "column count: gii={} niml={ncols}",
        columns.len()
    );

    for (col, da) in columns.iter().enumerate() {
        let label = dset.column_label(col);
        assert_eq!(
            da.data.len(),
            nrows,
            "col {col} ({label}): row count gii={} niml={nrows}",
            da.data.len()
        );

        let mut max_diff = 0.0f64;
        let mut worst_row = 0usize;
        for row in 0..nrows {
            let g = da.data.get_f64(row).unwrap_or(f64::NAN);
            let n = dset.data.get(row, col).unwrap();
            let diff = (g - n).abs();
            if diff > max_diff {
                max_diff = diff;
                worst_row = row;
            }
        }

        assert!(
            max_diff < 1e-5,
            "col {col} ({label}): max diff {max_diff:.2e} at row {worst_row} \
             (gii={:?} niml={})",
            da.data.get_f64(worst_row),
            dset.data.get(worst_row, col).unwrap()
        );
    }
}

#[test]
fn synthetic_gii_dsets_match_convertdset_dump() {
    for name in ["dense", "sparse"] {
        let rows = common::read_numeric_dump(&format!("surface/{name}_asc.gii.dset.dump.txt"));
        for encoding in ["asc", "b64", "b64gz"] {
            let gii = afni_io::gifti::Gifti::read(common::data(&format!(
                "surface/{name}_{encoding}.gii.dset"
            )))
            .unwrap();
            // Dump columns: node index, then one per data column. A sparse
            // GIfTI dset carries the node index as a leading NODE_INDEX array;
            // a dense one has none (row n is node n).
            let has_index = gii.data_arrays[0].intent == afni_io::gifti::intent::NODE_INDEX;
            assert_eq!(has_index, name == "sparse", "{name}_{encoding}: NODE_INDEX");
            let first_col = usize::from(!has_index);
            assert_eq!(
                gii.data_arrays.len() + first_col,
                rows[0].len(),
                "{name}_{encoding}: column count"
            );
            for (offset, da) in gii.data_arrays.iter().enumerate() {
                let col = offset + first_col;
                assert_eq!(da.data.len(), rows.len(), "{name}_{encoding} col {col}");
                for (row, expected) in rows.iter().enumerate() {
                    let actual = da.data.get_f64(row).unwrap();
                    assert!(
                        (actual - expected[col]).abs() < 1e-6,
                        "{name}_{encoding} [{row}, {col}]: {actual} vs {}",
                        expected[col]
                    );
                }
            }
        }
    }
}

#[test]
fn synthetic_gii_dsets_match_niml_dset() {
    let niml = common::data("surface/dense_asc.niml.dset");
    for encoding in ["asc", "b64", "b64gz"] {
        let gii = common::data(&format!("surface/dense_{encoding}.gii.dset"));
        assert_gii_matches_niml(&gii, &niml);
    }
}

#[test]
fn reference_isc_gii_dset_matches_niml_dset() {
    let (Some(gii), Some(niml)) = (
        common::reference("ISC_lh_theta_neg.gii.dset"),
        common::reference("ISC_lh_theta_neg.niml.dset"),
    ) else {
        return;
    };
    assert_gii_matches_niml(&gii, &niml);
}
