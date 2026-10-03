//! Statistic metadata: what kind of statistic a sub-brick or column holds and
//! its distribution parameters.
//!
//! AFNI's statistic codes are the NIfTI intent codes 2–24 (`NI_STAT_FIRSTCODE`
//! to `NI_STAT_LASTCODE` in `niml.h`; `NIFTI_INTENT_CORREL` to
//! `NIFTI_INTENT_LOG10PVAL`), so one [`StatSpec`] describes a statistic
//! wherever it comes from:
//!
//! | Source | Where | Via |
//! |--------|-------|-----|
//! | `.HEAD` | `BRICK_STATSYM`, else `BRICK_STATAUX` | [`crate::head::Header::brick_stats`] |
//! | NIfTI | the AFNI extension (same attributes), or the header intent | [`crate::nifti::Nifti::afni_header`], [`StatSpec::from_nifti_intent`] |
//! | `.niml.dset` | `COLMS_STATSYM` (same text as `BRICK_STATSYM`) | [`parse_statsym_list`] |
//! | GIfTI | each data array's `Intent` and `intent_p1..3` metadata | [`StatSpec::from_nifti_intent`] |
//!
//! Converting a statistic to a p-value is deliberately not here; that belongs
//! in a statistics crate (`afni-core`).
//!
//! References: `afni/src/niml/niml_stat.c` (`NI_stat_decode`,
//! `NI_stat_encode`), `3ddata.h` (`FUNC_*_TYPE`), `thd_initdblk.c`,
//! `thd_auxdata.c` (`THD_store_datablock_stataux`).

pub use afni_core::stat::{StatKind, StatSpec};

/// An FDR curve (`FDRCURVE_%06d`) or missed-detection curve (`MDFCURVE_%06d`)
/// for one sub-brick: samples of a function of the threshold, starting at
/// threshold `x0` and spaced `dx` apart. For an FDR curve the samples are
/// z(q) values (`thd_initdblk.c`, `thd_fdrcurve.c`).
#[derive(Debug, Clone, PartialEq)]
pub struct ThresholdCurve {
    /// Threshold of the first sample.
    pub x0: f64,
    /// Threshold spacing between samples.
    pub dx: f64,
    /// The samples.
    pub values: Vec<f64>,
}

impl ThresholdCurve {
    /// Decode the attribute layout `[x0, dx, v0, v1, ...]`. Like AFNI, needs
    /// more than three values.
    pub fn from_values(values: &[f64]) -> Option<Self> {
        (values.len() > 3).then(|| Self {
            x0: values[0],
            dx: values[1],
            values: values[2..].to_vec(),
        })
    }

    /// The attribute layout `[x0, dx, v0, v1, ...]`.
    pub fn to_values(&self) -> Vec<f64> {
        let mut out = vec![self.x0, self.dx];
        out.extend_from_slice(&self.values);
        out
    }
}

/// Split a `;`-separated STATSYM list (`BRICK_STATSYM`, `COLMS_STATSYM`)
/// into one entry per sub-brick or column, keeping positions: `none` or an
/// empty entry gives `None`.
pub fn parse_statsym_list(text: &str) -> Vec<Option<StatSpec>> {
    let text = text.trim();
    let text = text.strip_suffix(';').unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }
    text.split(';').map(StatSpec::parse).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statsym_lists_keep_positions() {
        let list = parse_statsym_list("Ttest(23);none;Ftest(2,40);");
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].as_ref().unwrap().kind, StatKind::Ttest);
        assert!(list[1].is_none());
        assert_eq!(list[2].as_ref().unwrap().params, [2.0, 40.0]);
        assert!(parse_statsym_list("").is_empty());
    }
}
