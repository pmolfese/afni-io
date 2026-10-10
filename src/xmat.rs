//! AFNI regression design matrices (`X.xmat.1D`).
//!
//! The numeric body is an ordinary row-major `.1D` table, but AFNI writes a
//! comment-wrapped NIML-like `<matrix>` header carrying column labels and
//! groups, timing, censoring, run boundaries, and stimulus ranges. This module
//! owns that file syntax. The validated, format-neutral meaning lives in
//! [`afni_core::design`].
//!
//! References: `3dDeconvolve.c` (`write_xsave`), `3dREMLfit.c` (`-matrix`),
//! and `afnipy/lib_afni1D.py`.

use std::path::Path;

use afni_core::design::{DesignMatrix, Regressor, RegressorRole, Stimulus};

use crate::error::{self, from_utf8, Error, Result};
use crate::onedee::OneD;

/// One unmodelled attribute from the `<matrix>` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmatAttribute {
    /// Attribute name as written.
    pub name: String,
    /// Decoded attribute value.
    pub value: String,
}

/// File-specific information retained beside a core design matrix.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmatExtras {
    /// Command that produced the matrix, when recorded.
    pub command_line: Option<String>,
    /// Header attributes this crate does not interpret, in file order.
    pub other_attributes: Vec<XmatAttribute>,
    /// Comment lines outside the `<matrix>` header, without the leading `#`.
    pub other_comments: Vec<String>,
}

/// A validated core design matrix plus AFNI-specific round-trip information.
#[derive(Debug, Clone, PartialEq)]
pub struct XmatFile {
    /// File-neutral design-matrix semantics.
    pub core: DesignMatrix,
    /// AFNI-specific metadata that core deliberately does not model.
    pub extras: XmatExtras,
}

impl XmatFile {
    /// Read and parse an AFNI X-matrix.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = error::read_file(path.as_ref())?;
        Self::parse(&from_utf8(&bytes, "X-matrix file")?)
    }

    /// Return whether text contains a comment-wrapped `<matrix>` header.
    ///
    /// Recognition is content-based, so an X-matrix need not have the usual
    /// `X.xmat.1D` filename.
    pub fn looks_like(text: &str) -> bool {
        text.lines().any(|line| {
            line.trim()
                .strip_prefix('#')
                .is_some_and(|comment| comment.trim().eq_ignore_ascii_case("<matrix"))
        })
    }

    /// Parse an AFNI X-matrix from text.
    pub fn parse(text: &str) -> Result<Self> {
        let table = OneD::parse(text)?;
        let (attributes, other_comments) = matrix_header(&table.comments)?;

        validate_declared_shape(&attributes, table.rows, table.cols)?;

        let labels = match attribute(&attributes, "ColumnLabels") {
            Some(value) => parse_labels(value),
            None => (0..table.cols)
                .map(|column| format!("col_{column}"))
                .collect(),
        };
        if labels.len() != table.cols {
            return Err(Error::invalid(format!(
                "X-matrix has {} ColumnLabels for {} columns",
                labels.len(),
                table.cols
            )));
        }

        let groups = attribute(&attributes, "ColumnGroups")
            .map(|value| decode_i64_list(value, None))
            .transpose()?
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| {
                        i32::try_from(value).map_err(|_| {
                            Error::invalid(format!("X-matrix column group {value} exceeds i32"))
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        if let Some(groups) = &groups {
            if groups.len() != table.cols {
                return Err(Error::invalid(format!(
                    "X-matrix has {} ColumnGroups for {} columns",
                    groups.len(),
                    table.cols
                )));
            }
        }
        let regressors = labels
            .into_iter()
            .enumerate()
            .map(|(column, label)| {
                let group = groups.as_ref().map(|groups| groups[column]);
                Regressor::new(label, role_for_group(group), group).map_err(core_err)
            })
            .collect::<Result<Vec<_>>>()?;

        let full_observations = attribute(&attributes, "NRowFull")
            .map(|value| parse_usize("NRowFull", value))
            .transpose()?
            .unwrap_or(table.rows);
        let retained_observations = match attribute(&attributes, "GoodList") {
            Some(value) => decode_usize_list("GoodList", value, Some(full_observations))?,
            None if full_observations == table.rows => (0..table.rows).collect(),
            None => {
                return Err(Error::missing(
                    "X-matrix GoodList for a censored/full-length mismatch",
                ))
            }
        };
        let run_starts = match attribute(&attributes, "RunStart") {
            Some(value) => decode_usize_list("RunStart", value, Some(full_observations))?,
            None => vec![0],
        };
        let sample_period_seconds = attribute(&attributes, "RowTR")
            .map(|value| {
                value
                    .parse::<f64>()
                    .map_err(|_| Error::parse(format!("X-matrix RowTR is not a number: {value:?}")))
            })
            .transpose()?;
        let stimuli = parse_stimuli(&attributes, table.cols)?;

        let core = DesignMatrix::new(
            table.rows,
            table.cols,
            table.values,
            regressors,
            full_observations,
            retained_observations,
            run_starts,
            sample_period_seconds,
            stimuli,
        )
        .map_err(core_err)?;
        let extras = XmatExtras {
            command_line: attribute(&attributes, "CommandLine").map(str::to_owned),
            other_attributes: attributes
                .into_iter()
                .filter(|attribute| !is_modelled_attribute(&attribute.name))
                .collect(),
            other_comments,
        };
        Ok(Self { core, extras })
    }

    /// Wrap a core matrix with no additional AFNI-specific metadata.
    pub fn from_core(core: DesignMatrix) -> Self {
        Self {
            core,
            extras: XmatExtras::default(),
        }
    }

    /// Serialize to AFNI's comment-wrapped X-matrix representation.
    pub fn to_xmat_string(&self) -> Result<String> {
        let mut output = String::new();
        output.push_str("# <matrix\n");
        write_attribute(
            &mut output,
            "ni_type",
            &format!("{}*double", self.core.regressor_count()),
        );
        write_attribute(
            &mut output,
            "ni_dimen",
            &self.core.observation_count().to_string(),
        );

        let labels = self
            .core
            .regressors()
            .iter()
            .map(|regressor| {
                if regressor.label().contains(';') {
                    Err(Error::invalid(format!(
                        "X-matrix label {:?} contains ';', the label separator",
                        regressor.label()
                    )))
                } else {
                    Ok(regressor.label())
                }
            })
            .collect::<Result<Vec<_>>>()?;
        write_attribute(&mut output, "ColumnLabels", &labels.join(" ; "));
        if self
            .core
            .regressors()
            .iter()
            .all(|regressor| regressor.group().is_some())
        {
            let groups = self
                .core
                .regressors()
                .iter()
                .map(|regressor| regressor.group().expect("checked Some").to_string())
                .collect::<Vec<_>>()
                .join(",");
            write_attribute(&mut output, "ColumnGroups", &groups);
        }
        if let Some(period) = self.core.sample_period_seconds() {
            write_attribute(&mut output, "RowTR", &crate::niml::format_float(period));
        }
        write_attribute(
            &mut output,
            "GoodList",
            &encode_usize_list(self.core.retained_observations()),
        );
        write_attribute(
            &mut output,
            "NRowFull",
            &self.core.full_observation_count().to_string(),
        );
        write_attribute(
            &mut output,
            "RunStart",
            &encode_usize_list(self.core.run_starts()),
        );
        if !self.core.stimuli().is_empty() {
            write_attribute(&mut output, "Nstim", &self.core.stimuli().len().to_string());
            write_attribute(
                &mut output,
                "StimBots",
                &self
                    .core
                    .stimuli()
                    .iter()
                    .map(|stimulus| stimulus.first_regressor().to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            write_attribute(
                &mut output,
                "StimTops",
                &self
                    .core
                    .stimuli()
                    .iter()
                    .map(|stimulus| stimulus.last_regressor().to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            write_attribute(
                &mut output,
                "StimLabels",
                &self
                    .core
                    .stimuli()
                    .iter()
                    .map(Stimulus::label)
                    .collect::<Vec<_>>()
                    .join(" ; "),
            );
        }
        if let Some(command) = &self.extras.command_line {
            write_attribute(&mut output, "CommandLine", command);
        }
        for attribute in &self.extras.other_attributes {
            if is_modelled_attribute(&attribute.name) {
                continue;
            }
            write_attribute(&mut output, &attribute.name, &attribute.value);
        }
        output.push_str("# >\n");
        for comment in &self.extras.other_comments {
            output.push_str("# ");
            output.push_str(comment);
            output.push('\n');
        }
        for row in 0..self.core.observation_count() {
            for column in 0..self.core.regressor_count() {
                if column > 0 {
                    output.push(' ');
                }
                output.push_str(&crate::niml::format_float(
                    self.core
                        .get(row, column)
                        .expect("validated matrix dimensions"),
                ));
            }
            output.push('\n');
        }
        Ok(output)
    }

    /// Write an AFNI-compatible X-matrix file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_xmat_string()?.as_bytes())
    }
}

fn matrix_header(comments: &[String]) -> Result<(Vec<XmatAttribute>, Vec<String>)> {
    let start = comments
        .iter()
        .position(|line| line.trim().eq_ignore_ascii_case("<matrix"))
        .ok_or_else(|| Error::parse("1D table has no <matrix header"))?;
    let end = comments[start + 1..]
        .iter()
        .position(|line| line.trim() == ">")
        .map(|position| start + 1 + position)
        .ok_or_else(|| Error::parse("X-matrix header has no closing >"))?;
    let mut attributes = Vec::new();
    for line in &comments[start + 1..end] {
        let Some((name, value)) = line.split_once('=') else {
            return Err(Error::parse(format!(
                "invalid X-matrix header line {line:?}"
            )));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::parse("X-matrix attribute has no name"));
        }
        attributes.push(XmatAttribute {
            name: name.to_owned(),
            value: decode_attribute_value(value.trim())?,
        });
    }
    let other_comments = comments
        .iter()
        .enumerate()
        .filter(|(index, _)| *index < start || *index > end)
        .map(|(_, line)| line.clone())
        .collect();
    Ok((attributes, other_comments))
}

fn decode_attribute_value(value: &str) -> Result<String> {
    let value = if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            &value[1..value.len() - 1]
        } else {
            value
        }
    } else {
        value
    };
    Ok(crate::niml::unescape(value))
}

fn attribute<'a>(attributes: &'a [XmatAttribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .rev()
        .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value.as_str())
}

fn validate_declared_shape(attributes: &[XmatAttribute], rows: usize, cols: usize) -> Result<()> {
    if let Some(value) = attribute(attributes, "ni_dimen") {
        let declared = value
            .split(',')
            .next()
            .ok_or_else(|| Error::parse("empty X-matrix ni_dimen"))?
            .trim()
            .parse::<usize>()
            .map_err(|_| Error::parse(format!("invalid X-matrix ni_dimen {value:?}")))?;
        if declared != rows {
            return Err(Error::invalid(format!(
                "X-matrix ni_dimen declares {declared} rows, body has {rows}"
            )));
        }
    }
    if let Some(value) = attribute(attributes, "ni_type") {
        let declared = match value.split_once('*') {
            Some((count, _)) => count
                .trim()
                .parse::<usize>()
                .map_err(|_| Error::parse(format!("invalid X-matrix ni_type {value:?}")))?,
            None => 1,
        };
        if declared != cols {
            return Err(Error::invalid(format!(
                "X-matrix ni_type declares {declared} columns, body has {cols}"
            )));
        }
    }
    Ok(())
}

fn parse_labels(value: &str) -> Vec<String> {
    let mut labels: Vec<String> = value
        .split(';')
        .map(|label| label.trim().to_owned())
        .collect();
    while labels.last().is_some_and(String::is_empty) {
        labels.pop();
    }
    labels
}

fn role_for_group(group: Option<i32>) -> RegressorRole {
    match group {
        Some(-1) => RegressorRole::Baseline,
        Some(0) => RegressorRole::Nuisance,
        Some(value) if value > 0 => RegressorRole::Interest,
        _ => RegressorRole::Unknown,
    }
}

fn parse_stimuli(attributes: &[XmatAttribute], cols: usize) -> Result<Vec<Stimulus>> {
    let count = attribute(attributes, "Nstim")
        .map(|value| parse_usize("Nstim", value))
        .transpose()?;
    let bots = attribute(attributes, "StimBots")
        .map(|value| decode_usize_list("StimBots", value, Some(cols)))
        .transpose()?;
    let tops = attribute(attributes, "StimTops")
        .map(|value| decode_usize_list("StimTops", value, Some(cols)))
        .transpose()?;
    let labels = attribute(attributes, "StimLabels").map(parse_labels);
    if count.is_none() && bots.is_none() && tops.is_none() && labels.is_none() {
        return Ok(Vec::new());
    }
    let count = count.ok_or_else(|| Error::missing("X-matrix Nstim"))?;
    let bots = bots.ok_or_else(|| Error::missing("X-matrix StimBots"))?;
    let tops = tops.ok_or_else(|| Error::missing("X-matrix StimTops"))?;
    let labels = labels.ok_or_else(|| Error::missing("X-matrix StimLabels"))?;
    for (name, found) in [
        ("StimBots", bots.len()),
        ("StimTops", tops.len()),
        ("StimLabels", labels.len()),
    ] {
        if found != count {
            return Err(Error::invalid(format!(
                "X-matrix {name} has {found} entries, Nstim is {count}"
            )));
        }
    }
    (0..count)
        .map(|index| Stimulus::new(&labels[index], bots[index], tops[index]).map_err(core_err))
        .collect()
}

fn parse_usize(name: &str, value: &str) -> Result<usize> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| Error::parse(format!("X-matrix {name} is not an integer: {value:?}")))
}

fn decode_usize_list(name: &str, value: &str, len: Option<usize>) -> Result<Vec<usize>> {
    decode_i64_list(value, len)
        .and_then(|values| {
            values
                .into_iter()
                .map(|value| {
                    usize::try_from(value).map_err(|_| {
                        Error::invalid(format!("X-matrix {name} contains negative value {value}"))
                    })
                })
                .collect()
        })
        .map_err(|error| Error::parse(format!("X-matrix {name}: {error}")))
}

fn decode_i64_list(value: &str, len: Option<usize>) -> Result<Vec<i64>> {
    let mut output = Vec::new();
    for raw_token in value.split(',') {
        let token = raw_token.trim();
        if token.is_empty() {
            return Err(Error::parse("empty entry in integer list"));
        }
        if let Some((count, repeated)) = token.split_once('@') {
            let count = count
                .trim()
                .parse::<usize>()
                .map_err(|_| Error::parse(format!("invalid repetition count {count:?}")))?;
            let repeated = parse_i64_endpoint(repeated.trim(), len)?;
            output.extend(std::iter::repeat(repeated).take(count));
            continue;
        }
        if let Some((start, rest)) = token.split_once("..") {
            let (end, requested_step) = if let Some(open) = rest.find('(') {
                if !rest.ends_with(')') {
                    return Err(Error::parse(format!("invalid integer range {token:?}")));
                }
                let step = rest[open + 1..rest.len() - 1]
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| Error::parse(format!("invalid range step in {token:?}")))?;
                (&rest[..open], Some(step))
            } else {
                (rest, None)
            };
            let start = parse_i64_endpoint(start.trim(), len)?;
            let end = parse_i64_endpoint(end.trim(), len)?;
            let direction = (end - start).signum();
            let step = requested_step.unwrap_or(if direction < 0 { -1 } else { 1 });
            if step == 0 || (direction != 0 && direction != step.signum()) {
                return Err(Error::invalid(format!(
                    "range step {step} points away from {start}..{end}"
                )));
            }
            let mut current = start;
            while if step > 0 {
                current <= end
            } else {
                current >= end
            } {
                output.push(current);
                if current == end {
                    break;
                }
                current = current
                    .checked_add(step)
                    .ok_or_else(|| Error::invalid("integer-list range overflows"))?;
            }
            continue;
        }
        output.push(parse_i64_endpoint(token, len)?);
    }
    Ok(output)
}

fn parse_i64_endpoint(value: &str, len: Option<usize>) -> Result<i64> {
    if value == "$" {
        let len = len.ok_or_else(|| Error::parse("'$' needs a known list bound"))?;
        let last = len
            .checked_sub(1)
            .ok_or_else(|| Error::invalid("'$' cannot address an empty list"))?;
        return i64::try_from(last).map_err(|_| Error::invalid("list bound exceeds i64"));
    }
    value
        .parse::<i64>()
        .map_err(|_| Error::parse(format!("invalid integer-list value {value:?}")))
}

fn encode_usize_list(values: &[usize]) -> String {
    values
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn is_modelled_attribute(name: &str) -> bool {
    [
        "ni_type",
        "ni_dimen",
        "ColumnLabels",
        "ColumnGroups",
        "RowTR",
        "GoodList",
        "NRowFull",
        "RunStart",
        "Nstim",
        "StimBots",
        "StimTops",
        "StimLabels",
        "CommandLine",
    ]
    .iter()
    .any(|known| name.eq_ignore_ascii_case(known))
}

fn write_attribute(output: &mut String, name: &str, value: &str) {
    output.push_str("#  ");
    output.push_str(name);
    output.push_str(" = \"");
    output.push_str(&crate::niml::escape(value));
    output.push_str("\"\n");
}

fn core_err(error: afni_core::Error) -> Error {
    Error::invalid(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# ordinary comment
# <matrix
#  ni_type = "3*double"
#  ni_dimen = "4"
#  ColumnLabels = "Run#1Pol#0 ; motion_roll ; visual#0"
#  ColumnGroups = "-1,0,1"
#  RowTR = "2"
#  GoodList = "0..1,3..4"
#  NRowFull = "5"
#  RunStart = "0,3"
#  Nstim = "1"
#  StimBots = "2"
#  StimTops = "2"
#  StimLabels = "visual"
#  CommandLine = "3dDeconvolve -x1D X.xmat.1D &amp;&amp; echo done"
#  BasisName = "BLOCK(1,1)"
# >
1 0 0
1 0.1 1
1 0.3 0
1 0.4 1
"#;

    #[test]
    fn recognizes_and_parses_semantics() {
        assert!(XmatFile::looks_like(SAMPLE));
        assert!(!XmatFile::looks_like("# numbers\n1 2\n"));
        let file = XmatFile::parse(SAMPLE).unwrap();
        assert_eq!(file.core.observation_count(), 4);
        assert_eq!(file.core.regressor_count(), 3);
        assert_eq!(file.core.full_observation_count(), 5);
        assert_eq!(file.core.retained_observations(), [0, 1, 3, 4]);
        assert_eq!(file.core.run_starts(), [0, 3]);
        assert_eq!(file.core.sample_period_seconds(), Some(2.0));
        assert_eq!(file.core.regressors()[0].role(), RegressorRole::Baseline);
        assert_eq!(file.core.regressors()[1].role(), RegressorRole::Nuisance);
        assert_eq!(file.core.regressors()[2].role(), RegressorRole::Interest);
        assert_eq!(file.core.stimuli()[0].label(), "visual");
        assert_eq!(
            file.extras.command_line.as_deref(),
            Some("3dDeconvolve -x1D X.xmat.1D && echo done")
        );
        assert_eq!(file.extras.other_attributes[0].name, "BasisName");
        assert_eq!(file.extras.other_comments, ["ordinary comment"]);
    }

    #[test]
    fn round_trips_core_and_unknown_metadata() {
        let parsed = XmatFile::parse(SAMPLE).unwrap();
        let text = parsed.to_xmat_string().unwrap();
        let reparsed = XmatFile::parse(&text).unwrap();
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn rejects_bad_shapes_and_censor_maps() {
        assert!(XmatFile::parse("# <matrix\n# ni_type = \"2*double\"\n# >\n1\n").is_err());
        let bad = SAMPLE.replace("0..1,3..4", "0..2");
        assert!(XmatFile::parse(&bad).is_err());
    }

    #[test]
    fn decodes_afni_integer_list_forms() {
        assert_eq!(
            decode_i64_list("3@-1,0,2..6(2),$,6..4", Some(8)).unwrap(),
            [-1, -1, -1, 0, 2, 4, 6, 7, 6, 5, 4]
        );
    }
}
