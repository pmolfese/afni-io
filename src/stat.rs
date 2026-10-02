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

/// A statistic's distribution: AFNI stat code = NIfTI intent code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StatKind {
    /// 2: correlation coefficient. Params: samples, fit parameters, ort parameters.
    Correl = 2,
    /// 3: Student t. Params: degrees of freedom.
    Ttest = 3,
    /// 4: F. Params: numerator and denominator degrees of freedom.
    Ftest = 4,
    /// 5: standard normal z.
    Zscore = 5,
    /// 6: chi-squared. Params: degrees of freedom.
    Chisq = 6,
    /// 7: beta. Params: a, b.
    Beta = 7,
    /// 8: binomial. Params: trials, probability per trial.
    Binom = 8,
    /// 9: gamma. Params: shape, scale.
    Gamma = 9,
    /// 10: Poisson. Params: mean.
    Poisson = 10,
    /// 11: normal. Params: mean, standard deviation.
    Normal = 11,
    /// 12: noncentral F. Params: numerator dof, denominator dof, noncentrality.
    FtestNonc = 12,
    /// 13: noncentral chi-squared. Params: dof, noncentrality.
    ChisqNonc = 13,
    /// 14: logistic. Params: location, scale.
    Logistic = 14,
    /// 15: Laplace. Params: location, scale.
    Laplace = 15,
    /// 16: uniform. Params: start, end.
    Uniform = 16,
    /// 17: noncentral t. Params: dof, noncentrality.
    TtestNonc = 17,
    /// 18: Weibull. Params: location, scale, power.
    Weibull = 18,
    /// 19: chi. Params: dof.
    Chi = 19,
    /// 20: inverse Gaussian. Params: mu, lambda.
    Invgauss = 20,
    /// 21: extreme value type I. Params: location, scale.
    Extval = 21,
    /// 22: the value is a p-value.
    Pval = 22,
    /// 23: the value is ln(p).
    LogPval = 23,
    /// 24: the value is log10(p).
    Log10Pval = 24,
}

const ALL: [StatKind; 23] = [
    StatKind::Correl,
    StatKind::Ttest,
    StatKind::Ftest,
    StatKind::Zscore,
    StatKind::Chisq,
    StatKind::Beta,
    StatKind::Binom,
    StatKind::Gamma,
    StatKind::Poisson,
    StatKind::Normal,
    StatKind::FtestNonc,
    StatKind::ChisqNonc,
    StatKind::Logistic,
    StatKind::Laplace,
    StatKind::Uniform,
    StatKind::TtestNonc,
    StatKind::Weibull,
    StatKind::Chi,
    StatKind::Invgauss,
    StatKind::Extval,
    StatKind::Pval,
    StatKind::LogPval,
    StatKind::Log10Pval,
];

impl StatKind {
    /// Map a stat code / NIfTI intent code (2–24) onto a variant.
    pub fn from_code(code: i64) -> Option<Self> {
        ALL.iter().copied().find(|k| k.code() == code)
    }

    /// The stat code, which is also the NIfTI intent code.
    pub fn code(self) -> i64 {
        self as i64
    }

    /// The name used in `BRICK_STATSYM` / `COLMS_STATSYM` strings, e.g.
    /// `Ttest` (`distname` in `niml_stat.c`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Correl => "Correl",
            Self::Ttest => "Ttest",
            Self::Ftest => "Ftest",
            Self::Zscore => "Zscore",
            Self::Chisq => "Chisq",
            Self::Beta => "Beta",
            Self::Binom => "Binom",
            Self::Gamma => "Gamma",
            Self::Poisson => "Poisson",
            Self::Normal => "Normal",
            Self::FtestNonc => "Ftest_nonc",
            Self::ChisqNonc => "Chisq_nonc",
            Self::Logistic => "Logistic",
            Self::Laplace => "Laplace",
            Self::Uniform => "Uniform",
            Self::TtestNonc => "Ttest_nonc",
            Self::Weibull => "Weibull",
            Self::Chi => "Chi",
            Self::Invgauss => "Invgauss",
            Self::Extval => "Extval",
            Self::Pval => "Pval",
            Self::LogPval => "LogPval",
            Self::Log10Pval => "Log10Pval",
        }
    }

    /// Number of distribution parameters (`numparam` in `niml_stat.c`).
    pub fn num_params(self) -> usize {
        match self {
            Self::Zscore | Self::Pval | Self::LogPval | Self::Log10Pval => 0,
            Self::Ttest | Self::Chisq | Self::Poisson | Self::Chi => 1,
            Self::Correl | Self::FtestNonc | Self::Weibull => 3,
            _ => 2,
        }
    }

    /// Whether AFNI's `.HEAD` loader keeps this code. It stores only the
    /// classic `FUNC_*_TYPE` codes 2–10 (`FUNC_IS_STAT`, `3ddata.h`) and
    /// drops the rest; this crate keeps all of them.
    pub fn is_afni_brick_stat(self) -> bool {
        self.code() <= 10
    }
}

/// A statistic and its parameters, e.g. `Ttest(23)`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatSpec {
    /// The distribution.
    pub kind: StatKind,
    /// Exactly [`StatKind::num_params`] parameters.
    pub params: Vec<f64>,
}

impl StatSpec {
    /// Build a spec from however many parameters are available: extras are
    /// dropped and missing ones are set to `fill`. (AFNI fills with 1.0 when
    /// decoding a STATSYM string and with 0.0 for `BRICK_STATAUX`.)
    pub fn new(kind: StatKind, params: &[f64], fill: f64) -> Self {
        let n = kind.num_params();
        let mut params: Vec<f64> = params.iter().copied().take(n).collect();
        params.resize(n, fill);
        Self { kind, params }
    }

    /// Parse one STATSYM entry such as `Ttest(23)` or `Ftest(2,40)`, as
    /// `NI_stat_decode` does: the name is matched case-insensitively, and a
    /// missing or unreadable parameter becomes 1.0. `none`, an empty string or
    /// an unknown name gives `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let bytes = text.as_bytes();
        let kind = ALL.iter().copied().find(|k| {
            let n = k.name().len();
            bytes.len() > n
                && bytes[..n].eq_ignore_ascii_case(k.name().as_bytes())
                && bytes[n] == b'('
        })?;
        let args = &text[kind.name().len() + 1..];
        let values: Vec<f64> = args
            .split([',', ')'])
            .take(kind.num_params())
            .map(|v| v.trim().parse().unwrap_or(1.0))
            .collect();
        Some(Self::new(kind, &values, 1.0))
    }

    /// Format as AFNI writes it (`NI_stat_encode`), e.g. `Ftest(2,40)` or
    /// `Zscore()`.
    pub fn to_statsym(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|&v| format_param(v as f32))
            .collect();
        format!("{}({})", self.kind.name(), params.join(","))
    }

    /// The statistic described by a NIfTI intent code and its parameters
    /// (`intent_p1..3`), or `None` when the code is not a statistic.
    pub fn from_nifti_intent(code: i64, params: [f64; 3]) -> Option<Self> {
        Some(Self::new(StatKind::from_code(code)?, &params, 0.0))
    }
}

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

/// `NI_fval_to_char` from `niml_stat.c`: integers print as integers, other
/// values with a precision chosen by magnitude and trailing zeros removed.
/// (AFNI pads its `%-9.0f` case with trailing blanks; they are trimmed here.)
fn format_param(v: f32) -> String {
    if v == 0.0 {
        return "0".into();
    }
    if v.abs() < 99_999_999.0 && v == v.trunc() {
        return format!("{}", v as i64);
    }
    let strip = |s: String| {
        let trimmed = s.trim_end_matches('0');
        if trimmed.len() > 1 {
            trimmed.to_string()
        } else {
            s
        }
    };
    let lv = (10.0001 + f64::from(v.abs()).log10()) as i64;
    let v64 = f64::from(v);
    match lv {
        6..=10 => strip(format!("{v64:.6}")),
        11 => strip(format!("{v64:.5}")),
        12 => strip(format!("{v64:.4}")),
        13 => strip(format!("{v64:.3}")),
        14 => strip(format!("{v64:.2}")),
        15 => strip(format!("{v64:.1}")),
        16 => format!("{v64:.0}"),
        _ => c_exponent(v64, if v > 0.0 { 6 } else { 5 }),
    }
}

/// C's `%.Ne`: mantissa with `digits` decimals and a signed exponent of at
/// least two digits, e.g. `1.500000e-05`.
fn c_exponent(v: f64, digits: usize) -> String {
    let s = format!("{v:.digits$e}");
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exp.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_names_and_parameter_counts_match_niml_stat() {
        // numparam[] from niml_stat.c, codes 2..=24.
        let numparam = [
            3, 1, 2, 0, 1, 2, 2, 2, 1, 2, 3, 2, 2, 2, 2, 2, 3, 1, 2, 2, 0, 0, 0,
        ];
        for (i, kind) in ALL.iter().enumerate() {
            assert_eq!(kind.code(), i as i64 + 2);
            assert_eq!(kind.num_params(), numparam[i], "{kind:?}");
            assert_eq!(StatKind::from_code(kind.code()), Some(*kind));
        }
        assert_eq!(StatKind::from_code(1), None);
        assert_eq!(StatKind::from_code(25), None);
        assert!(StatKind::Poisson.is_afni_brick_stat() && !StatKind::Normal.is_afni_brick_stat());
    }

    #[test]
    fn parses_like_ni_stat_decode() {
        let t = StatSpec::parse("Ttest(23)").unwrap();
        assert_eq!(
            (t.kind, t.params.as_slice()),
            (StatKind::Ttest, &[23.0][..])
        );
        let f = StatSpec::parse(" ftest(2, 40) ").unwrap();
        assert_eq!(
            (f.kind, f.params.as_slice()),
            (StatKind::Ftest, &[2.0, 40.0][..])
        );
        // Ttest_nonc is not mistaken for Ttest; missing parameters become 1.0.
        let nc = StatSpec::parse("Ttest_nonc(12)").unwrap();
        assert_eq!(
            (nc.kind, nc.params.as_slice()),
            (StatKind::TtestNonc, &[12.0, 1.0][..])
        );
        assert_eq!(
            StatSpec::parse("Zscore()").unwrap().params,
            Vec::<f64>::new()
        );
        assert_eq!(StatSpec::parse("none"), None);
        assert_eq!(StatSpec::parse("Ttest"), None);
        assert_eq!(StatSpec::parse("Bogus(1)"), None);
    }

    #[test]
    fn encodes_like_ni_stat_encode() {
        let enc = |kind, params: &[f64]| StatSpec::new(kind, params, 0.0).to_statsym();
        assert_eq!(enc(StatKind::Ttest, &[23.0]), "Ttest(23)");
        assert_eq!(enc(StatKind::Ftest, &[2.0, 40.0]), "Ftest(2,40)");
        assert_eq!(enc(StatKind::Zscore, &[]), "Zscore()");
        assert_eq!(enc(StatKind::Correl, &[30.0, 2.0, 1.0]), "Correl(30,2,1)");
        assert_eq!(enc(StatKind::Binom, &[10.0, 0.25]), "Binom(10,0.25)");
        assert_eq!(enc(StatKind::Ttest, &[12.5]), "Ttest(12.5)");
        assert_eq!(
            enc(StatKind::Normal, &[0.00001, -2.5e-7]),
            "Normal(1.000000e-05,-2.50000e-07)"
        );
        for text in ["Ttest(23)", "Ftest(2,40)", "Binom(10,0.25)", "Zscore()"] {
            assert_eq!(StatSpec::parse(text).unwrap().to_statsym(), text);
        }
    }

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
