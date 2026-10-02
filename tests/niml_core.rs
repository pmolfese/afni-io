//! NIML core: typed columns, variable-length records, the binary writer and
//! incremental (streaming) parsing.

mod common;

use afni_io::array::{DataType, TypedArray};
use afni_io::niml::{
    self, parse_bytes, parse_stream, serialize, serialize_binary, NimlData, NimlElement,
    NimlValueType, RecordTable,
};

/// Every committed NIML fixture: AFNI-written datasets (ASCII and binary),
/// SUMA and sumaru ROIs, label tables, and sumaru's cluster dataset.
fn niml_fixtures() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for dir in ["surface", "real/roi", "real/labels", "real/dset"] {
        for entry in std::fs::read_dir(common::data(dir)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.to_string_lossy().into_owned();
            if name.ends_with(".niml.dset")
                || name.ends_with(".niml.roi")
                || name.ends_with(".niml.lt")
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    assert!(paths.len() >= 25, "{paths:?}");
    paths
}

/// Elements with the attributes the writer derives from the body removed,
/// so trees written different ways can be compared.
fn normalized(elements: &[NimlElement]) -> Vec<NimlElement> {
    elements
        .iter()
        .map(|e| {
            let mut e = e.clone();
            e.attrs.remove("ni_form");
            if let NimlData::Group(children) = &e.data {
                e.data = NimlData::Group(normalized(children));
            }
            e
        })
        .collect()
}

#[test]
fn columns_keep_their_declared_types() {
    // ConvertDset wrote the same data as ASCII and as binary floats.
    let asc = niml::read(common::data("surface/dense_asc.niml.dset")).unwrap();
    let bin = niml::read(common::data("surface/dense_bi.niml.dset")).unwrap();
    let data = |e: &[NimlElement]| match &e[0].child("SPARSE_DATA").unwrap().data {
        NimlData::Numeric(m) => m.clone(),
        other => panic!("{other:?}"),
    };
    let (a, b) = (data(&asc), data(&bin));
    assert_eq!(a, b);
    assert_eq!(a.column_types(), vec![NimlValueType::Float32; 3]);
    assert!(matches!(a.columns[0], TypedArray::Float32(_)));
    assert_eq!(a.get(41, 2), Some(241.0));

    // The INDEX_LIST is an int column.
    let NimlData::Numeric(index) = &asc[0].child("INDEX_LIST").unwrap().data else {
        panic!();
    };
    assert_eq!(index.columns[0].dtype(), DataType::Int32);
}

#[test]
fn every_fixture_round_trips_through_binary_and_ascii() {
    for path in niml_fixtures() {
        let original = niml::read(&path).unwrap();
        let from_binary = parse_bytes(&serialize_binary(&original))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            normalized(&from_binary),
            normalized(&original),
            "{}",
            path.display()
        );
        // Writing a binary-read tree as ASCII drops its binary ni_form.
        let from_ascii = parse_bytes(serialize(&from_binary).as_bytes())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            normalized(&from_ascii),
            normalized(&original),
            "{}",
            path.display()
        );
    }
}

#[test]
fn roi_and_tract_records_in_ascii_and_binary() {
    // A real SUMA ROI: strokes are SUMA_NIML_ROI_DATUM records.
    let roi = niml::read(common::data("real/roi/demo.lh.3.filled.niml.roi")).unwrap();
    let NimlData::Records(table) = &roi[0].data else {
        panic!("{:?}", roi[0].data);
    };
    assert_eq!(table.record_type, NimlValueType::SumaRoiDatum);
    for record in &table.rows {
        assert_eq!(
            record[2][0] as usize,
            record[3].len(),
            "count matches nodes"
        );
    }
    let bytes = serialize_binary(&roi);
    assert!(String::from_utf8_lossy(&bytes).contains("binary.lsbfirst"));
    assert_eq!(normalized(&parse_bytes(&bytes).unwrap()), normalized(&roi));

    // Tracts: id, number of floats, then x y z per point (TAYLOR_TRACT_DATUM).
    let tracts = NimlElement {
        name: "tracts".into(),
        attrs: Default::default(),
        data: NimlData::Records(RecordTable {
            record_type: NimlValueType::TaylorTractDatum,
            rows: vec![
                // The length field is wrong on purpose: writers set it.
                vec![vec![7.0], vec![99.0], vec![1.0, 2.0, 3.0, 4.5, 5.5, 6.5]],
                vec![vec![8.0], vec![0.0], vec![-1.25, 0.0, 2.0]],
            ],
        }),
    };
    let ascii = serialize(std::slice::from_ref(&tracts));
    assert!(
        ascii.contains("\n7 6 1.0 2.0 3.0 4.5 5.5 6.5\n8 3 -1.25 0.0 2.0\n"),
        "{ascii}"
    );
    for bytes in [ascii.into_bytes(), serialize_binary(&[tracts])] {
        let NimlData::Records(back) = &parse_bytes(&bytes).unwrap()[0].data else {
            panic!();
        };
        assert_eq!(back.rows[0][1], [6.0]);
        assert_eq!(back.rows[1][2], [-1.25, 0.0, 2.0]);
    }
}

#[test]
fn parse_stream_waits_for_complete_elements() {
    // A stream like an AFNI socket's: ASCII and binary elements, processing
    // instructions, and blanks between them.
    let mut stream = b"<?ni_do ni_verb='DRIVE_AFNI' ?>\n".to_vec();
    let mut expected = Vec::new();
    for name in [
        "surface/dense_bi.niml.dset",
        "real/roi/test.roi1_roi2.niml.roi",
        "surface/stat.niml.dset",
    ] {
        let elements = niml::read(common::data(name)).unwrap();
        stream.extend_from_slice(&serialize_binary(&elements));
        stream.extend_from_slice(b"\n  ");
        expected.extend(elements);
    }
    let whole = parse_bytes(&stream).unwrap();
    assert_eq!(normalized(&whole), normalized(&expected));

    // One byte at a time, and in uneven chunks: the same elements, in order.
    for chunk in [1, 7, 4096] {
        let mut buffer = Vec::new();
        let mut got = Vec::new();
        for piece in stream.chunks(chunk) {
            buffer.extend_from_slice(piece);
            let (elements, consumed) = parse_stream(&buffer).unwrap();
            got.extend(elements);
            buffer.drain(..consumed);
        }
        assert!(
            buffer.is_empty(),
            "chunk {chunk}: {} bytes left",
            buffer.len()
        );
        assert_eq!(got, whole, "chunk size {chunk}");
    }

    // A partial element is left for later; nothing is lost.
    let cut = stream.len() - 10;
    let (elements, consumed) = parse_stream(&stream[..cut]).unwrap();
    assert_eq!(elements.len(), whole.len() - 1);
    assert!(consumed < cut);

    // Input that can never become NIML is an error...
    assert!(parse_stream(b"</a>").is_err());
    assert!(parse_stream(b"not niml").is_err());
    assert!(parse_stream(b"<a ni_type=\"int\" ni_dimen=\"1\" >zz</a>").is_err());
    // ...but a missing closing tag might still arrive, so it waits.
    assert_eq!(parse_stream(b"<a>text</b>").unwrap(), (vec![], 0));
}
