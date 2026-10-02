//! Inspect an AFNI/SUMA file, dispatching on its extension.
//!
//! ```text
//! cargo run --example inspect -- path/to/file
//! ```

use std::path::Path;

use afni_io::prelude::*;

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: inspect <file>");
        std::process::exit(2);
    };
    if let Err(err) = inspect(Path::new(&arg)) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn inspect(path: &Path) -> Result<()> {
    let name = path.to_string_lossy().to_lowercase();

    if name.ends_with(".niml.dset") {
        let dset = NimlDataset::read(path)?;
        println!("niml.dset  type={}", dset.dset_type);
        println!("  {} nodes x {} sub-bricks", dset.rows(), dset.columns());
        println!("  sparse: {}", dset.is_sparse());
        for c in 0..dset.columns() {
            println!("  col {c}: {}", dset.column_label(c));
        }
    } else if name.ends_with(".niml.roi") {
        let rois = NodeRoi::read_all(path)?;
        println!("niml.roi  {} ROI(s)", rois.len());
        for roi in &rois {
            println!(
                "  {} (iLabel {}): {} strokes, {} unique nodes",
                roi.label,
                roi.integer_label,
                roi.data.len(),
                roi.unique_nodes().len()
            );
        }
    } else if AfniPaths::is_afni_name(path) {
        let brik = Brik::read(path)?;
        let [nx, ny, nz] = brik.dimensions;
        println!("HEAD/BRIK  {nx}x{ny}x{nz} x {} sub-bricks", brik.nvals());
        for (p, sub) in brik.sub_bricks.iter().flatten().enumerate() {
            println!("  [{p}] {:?}, factor {}", sub.brik_type(), sub.factor);
        }
        if let Some(ts) = brik.header.typestring() {
            println!("  typestring: {ts}");
        }
        if let Some(d) = brik.header.delta() {
            println!("  voxel size: {:?} mm", d);
        }
        let h = &brik.header;
        println!(
            "  orient: {}  view: {}  obliquity: {:.3} deg",
            h.orientation_string().unwrap_or_default(),
            h.view().map_or("?", |v| v.suffix()),
            h.obliquity()
        );
        if let Ok(m) = h.ijk_to_dicom() {
            println!("  ijk_to_dicom (RAI):");
            for row in &m[..3] {
                println!(
                    "    {:10.4} {:10.4} {:10.4} {:10.4}",
                    row[0], row[1], row[2], row[3]
                );
            }
        }
        if let Some(t) = h.time_axis() {
            println!(
                "  time axis: nt={} TR={:?} s, {} slice offsets",
                t.nt,
                t.tr_seconds(),
                t.slice_offsets.len()
            );
        }
        for (p, label) in brik.header.brick_labels().iter().enumerate() {
            println!("  [{p}] {label}");
        }
    } else if name.ends_with(".spec") {
        let spec = Spec::read(path)?;
        println!("spec  {} surface(s)", spec.surfaces.len());
        for s in &spec.surfaces {
            println!(
                "  {} (state: {})",
                s.surface_name().unwrap_or("?"),
                s.state().unwrap_or("?")
            );
        }
    } else if name.ends_with(".asc") {
        let surf = Surface::read(path)?;
        println!(
            "surface  {} vertices, {} faces",
            surf.n_vertices(),
            surf.n_faces()
        );
    } else if name.ends_with(".gii")
        || name.ends_with(".gii.gz")
        || name.ends_with(".gii.dset")
        || name.ends_with(".gii.dset.gz")
    {
        let gii = Gifti::read(path)?;
        println!(
            "gifti  version {}, {} data array(s)",
            gii.version,
            gii.data_arrays.len()
        );
        for a in &gii.data_arrays {
            let intent = afni_io::gifti::intent::name_for_code(a.intent)
                .map(str::to_string)
                .unwrap_or_else(|| a.intent.to_string());
            println!("  {intent}: {:?} {} elems", a.dims, a.data.len());
        }
        if let (Some(v), Some(f)) = (gii.pointset(), gii.triangles()) {
            println!("  surface: {} vertices, {} faces", v.len(), f.len());
        }
    } else if name.ends_with(".nii")
        || name.ends_with(".nii.gz")
        || name.ends_with(".hdr")
        || name.ends_with(".img")
    {
        let vol = Nifti::read(path)?;
        println!(
            "nifti  {:?}  shape {:?}  dtype {}",
            vol.header.version,
            vol.shape(),
            vol.header.data_type().map(|d| d.as_name()).unwrap_or("?")
        );
        if !vol.header.descrip.is_empty() {
            println!("  descrip: {}", vol.header.descrip);
        }
        println!("  affine row0: {:?}", vol.header.affine()[0]);
    } else if name.ends_with(".1d") {
        let one = OneD::read(path)?;
        println!("1D  {} rows x {} cols", one.rows, one.cols);
    } else if name.ends_with(".niml") || name.ends_with(".niml.asc") {
        let elements = afni_io::niml::read(path)?;
        println!("niml  {} top-level element(s)", elements.len());
        for e in &elements {
            println!("  <{}> ({} children)", e.name, e.children().len());
        }
    } else {
        eprintln!("unrecognized extension: {}", path.display());
        std::process::exit(2);
    }
    Ok(())
}
