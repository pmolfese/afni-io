//! Label tables to colors: AFNI/SUMA tables read from real files keep their
//! integer keys, order and exact colors through `afni-core`, and keys without
//! colors get the documented fallbacks.

mod common;

use afni_core::color::Rgba;
use afni_core::labels::{
    stable_label_rgb, KeyColor, LabelColorMap, LabelColorPolicy, ZeroKeyPolicy,
};
use afni_io::adapt::{label_table_from_core, label_table_to_core};
use afni_io::prelude::*;

fn labels_dset() -> LabelTable {
    NimlDataset::read(common::data("surface/labels.niml.dset"))
        .unwrap()
        .label_table
        .expect("label dataset has a table")
}

#[test]
fn surface_label_table_keeps_keys_order_and_exact_colors() {
    let raw = labels_dset();
    let core = label_table_to_core(&raw).unwrap();
    assert_eq!(core.len(), raw.entries.len());
    let map = LabelColorMap::with_defaults(core);
    for (position, entry) in raw.entries.iter().enumerate() {
        // Same order, same integer key, same name.
        let mine = &map.table().entries()[position];
        assert_eq!((mine.key, &mine.name), (entry.key, &entry.name));
        // Same color, bit for bit: never re-quantized.
        let want = entry.rgba.expect("AFNI_labeltable entries have colors");
        assert_eq!(
            map.color_for_key(entry.key).to_array(),
            want,
            "key {}",
            entry.key
        );
    }
    // Colors in the file are 8-bit fractions (0.3 -> 76/255).
    let first = map.color_for_key(1).to_u8();
    assert_eq!(first[0], 76);
}

#[test]
fn keys_missing_from_the_table_are_unlabeled() {
    let map = LabelColorMap::with_defaults(label_table_to_core(&labels_dset()).unwrap());
    assert_eq!(map.color_for_key(12345), Rgba::TRANSPARENT);
    let hidden_zero = LabelColorMap::new(
        label_table_to_core(&labels_dset()).unwrap(),
        LabelColorPolicy {
            zero: ZeroKeyPolicy::Unlabeled,
            ..LabelColorPolicy::default()
        },
    );
    // Key 0 ("undefined" in this table) is hidden under that policy only.
    assert_eq!(hidden_zero.color_for_key(0), Rgba::TRANSPARENT);
    assert_ne!(map.color_for_key(0), Rgba::TRANSPARENT);
}

#[test]
fn volume_label_table_has_no_colors_so_the_stable_palette_applies() {
    // `3drefit -labeltable` tables (VALUE_LABEL_DTABLE) carry names but no colors.
    let raw = LabelTable::read(common::data("real/labels/aparc+aseg_REN_all.niml.lt")).unwrap();
    assert!(raw.entries.iter().all(|e| e.rgba.is_none()));
    let core = match label_table_to_core(&raw) {
        Ok(t) => t,
        Err(e) => panic!("a real FreeSurfer table failed to convert: {e}"),
    };
    assert_eq!(core.len(), raw.entries.len());
    assert!(core.len() > 50, "the aparc+aseg table has many regions");
    let map = LabelColorMap::with_defaults(core);
    for entry in raw.entries.iter().take(40) {
        let [r, g, b] = stable_label_rgb(entry.key);
        assert_eq!(
            map.color_for_key(entry.key),
            Rgba::from_u8(r, g, b, 255),
            "key {}",
            entry.key
        );
    }
    // Each key in the table keeps its name (lookup by integer, not float).
    let probe = &raw.entries[raw.entries.len() / 2];
    assert_eq!(map.table().get(probe.key).unwrap().name, probe.name);
    // A policy can instead leave uncolored regions unpainted.
    let plain = LabelColorMap::new(
        map.table().clone(),
        LabelColorPolicy {
            uncolored: KeyColor::Fixed(Rgba::TRANSPARENT),
            ..LabelColorPolicy::default()
        },
    );
    assert_eq!(plain.color_for_key(probe.key), Rgba::TRANSPARENT);
}

#[test]
fn core_label_table_converts_back_without_loss() {
    let raw = labels_dset();
    let core = label_table_to_core(&raw).unwrap();
    let back = label_table_from_core(&core, raw.attrs.clone());
    assert_eq!(back, raw);
}

#[test]
fn duplicate_keys_in_a_file_are_an_error_not_a_silent_merge() {
    let mut raw = labels_dset();
    let dup = raw.entries[0].clone();
    raw.entries.push(dup);
    assert!(label_table_to_core(&raw).is_err());
}
