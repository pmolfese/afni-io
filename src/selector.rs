//! AFNI dataset names with trailing sub-brick selectors.
//!
//! AFNI accepts names such as `epi+orig[0,2..10(2),$]`. Selection is ordered,
//! inclusive, and may contain duplicates. Endpoints may also be sub-brick
//! labels; when several labels share a prefix, AFNI chooses the longest match.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::{Error, Result};

/// A filesystem path plus an optional AFNI sub-brick selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetSpec {
    path: PathBuf,
    selector: Option<SubBrickSelector>,
}

impl DatasetSpec {
    /// Parse a dataset name, separating a terminal `[...]` selector.
    pub fn parse(text: &str) -> Result<Self> {
        if !text.ends_with(']') {
            return Ok(Self {
                path: PathBuf::from(text),
                selector: None,
            });
        }
        let open = text.rfind('[').ok_or_else(|| {
            Error::parse(format!("dataset selector has no opening '[': {text:?}"))
        })?;
        let path = &text[..open];
        if path.is_empty() {
            return Err(Error::parse("dataset selector has no path"));
        }
        let selector = SubBrickSelector::parse(&text[open + 1..text.len() - 1])?;
        Ok(Self {
            path: PathBuf::from(path),
            selector: Some(selector),
        })
    }

    /// Interpret a path as an AFNI dataset specification when it is UTF-8.
    /// Non-UTF-8 paths remain ordinary paths without a selector.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match path.to_str() {
            Some(text) => Self::parse(text),
            None => Ok(Self {
                path: path.to_path_buf(),
                selector: None,
            }),
        }
    }

    /// The path with the selector removed.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The optional sub-brick selector.
    pub fn selector(&self) -> Option<&SubBrickSelector> {
        self.selector.as_ref()
    }

    /// Resolve the selector, or all sub-bricks when no selector was present.
    pub fn resolve(&self, nvals: usize, labels: &[String]) -> Result<Vec<usize>> {
        match &self.selector {
            Some(selector) => selector.resolve(nvals, labels),
            None => Ok((0..nvals).collect()),
        }
    }
}

impl FromStr for DatasetSpec {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl fmt::Display for DatasetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(selector) = &self.selector {
            write!(f, "[{selector}]")?;
        }
        Ok(())
    }
}

/// An unresolved AFNI sub-brick selector (the contents of trailing `[...]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubBrickSelector {
    expression: String,
}

impl SubBrickSelector {
    /// Parse and perform syntax checks that do not require dataset metadata.
    pub fn parse(expression: &str) -> Result<Self> {
        let expression = expression.trim();
        if expression.is_empty() {
            return Err(Error::parse("empty sub-brick selector"));
        }
        let mut depth = 0usize;
        for character in expression.chars() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or_else(|| Error::parse("unmatched ')' in sub-brick selector"))?;
                }
                _ => {}
            }
        }
        if depth != 0 {
            return Err(Error::parse("unmatched '(' in sub-brick selector"));
        }
        Ok(Self {
            expression: expression.to_owned(),
        })
    }

    /// The selector text without surrounding brackets.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// Resolve indices using a dataset's sub-brick count and labels.
    ///
    /// This follows `MCW_get_labels_intlist`: ranges are inclusive, order and
    /// duplicates are retained, `$` means `nvals - 1`, and labels containing
    /// spaces may be written with underscores.
    pub fn resolve(&self, nvals: usize, labels: &[String]) -> Result<Vec<usize>> {
        if nvals == 0 {
            return Err(Error::invalid(
                "cannot select from a dataset with no sub-bricks",
            ));
        }
        let text = self.expression.as_str();
        let mut position = 0usize;
        let mut output = Vec::new();

        while position < text.len() {
            skip_spaces(text, &mut position);
            let start = parse_endpoint(text, &mut position, nvals, labels)?;
            skip_spaces(text, &mut position);

            if position == text.len() || text[position..].starts_with(',') {
                output.push(start);
            } else {
                if text[position..].starts_with("..") {
                    position += 2;
                } else if text[position..].starts_with('-') || text[position..].starts_with(':') {
                    position += 1;
                } else {
                    return Err(Error::parse(format!(
                        "expected ',', '..', '-' or ':' at {:?}",
                        &text[position..]
                    )));
                }
                skip_spaces(text, &mut position);
                let end = parse_endpoint(text, &mut position, nvals, labels)?;
                skip_spaces(text, &mut position);

                let direction = if start <= end { 1isize } else { -1isize };
                let mut step = direction;
                if position < text.len() && text[position..].starts_with('(') {
                    position += 1;
                    skip_spaces(text, &mut position);
                    step = parse_signed_step(text, &mut position)?;
                    skip_spaces(text, &mut position);
                    if position >= text.len() || !text[position..].starts_with(')') {
                        return Err(Error::parse("selector stride has no closing ')'"));
                    }
                    position += 1;
                    if step.signum() != direction {
                        return Err(Error::invalid(format!(
                            "selector stride {step} points away from range {start}..{end}"
                        )));
                    }
                }

                let mut value = start as isize;
                let end = end as isize;
                while (value - end) * step <= 0 {
                    output.push(value as usize);
                    value = value
                        .checked_add(step)
                        .ok_or_else(|| Error::invalid("sub-brick selector range overflows"))?;
                }
            }

            skip_spaces(text, &mut position);
            if position == text.len() {
                break;
            }
            if !text[position..].starts_with(',') {
                return Err(Error::parse(format!(
                    "expected ',' at {:?}",
                    &text[position..]
                )));
            }
            position += 1;
            skip_spaces(text, &mut position);
            if position == text.len() {
                return Err(Error::parse("sub-brick selector ends with ','"));
            }
        }
        Ok(output)
    }
}

impl FromStr for SubBrickSelector {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl fmt::Display for SubBrickSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.expression)
    }
}

fn skip_spaces(text: &str, position: &mut usize) {
    while *position < text.len() && text.as_bytes()[*position].is_ascii_whitespace() {
        *position += 1;
    }
}

fn parse_endpoint(
    text: &str,
    position: &mut usize,
    nvals: usize,
    labels: &[String],
) -> Result<usize> {
    if text[*position..].starts_with('$') {
        *position += 1;
        return Ok(nvals - 1);
    }

    // AFNI tries labels before integers and keeps the longest prefix match.
    if let Some((index, bytes)) = longest_label_prefix(&text[*position..], labels, false)
        .or_else(|| longest_label_prefix(&text[*position..], labels, true))
    {
        *position += bytes;
        if index >= nvals {
            return Err(Error::invalid(format!(
                "label resolves to sub-brick {index}, outside 0..{}",
                nvals - 1
            )));
        }
        return Ok(index);
    }

    let start = *position;
    while *position < text.len() && text.as_bytes()[*position].is_ascii_digit() {
        *position += 1;
    }
    if *position == start {
        return Err(Error::parse(format!(
            "expected sub-brick index, '$', or label at {:?}",
            &text[start..]
        )));
    }
    let index = text[start..*position]
        .parse::<usize>()
        .map_err(|_| Error::parse("invalid sub-brick index"))?;
    if index >= nvals {
        return Err(Error::invalid(format!(
            "sub-brick {index} is outside 0..{}",
            nvals - 1
        )));
    }
    Ok(index)
}

fn longest_label_prefix(
    input: &str,
    labels: &[String],
    spaces_as_underscores: bool,
) -> Option<(usize, usize)> {
    let mut best = None;
    for (index, label) in labels.iter().enumerate() {
        let candidate = if spaces_as_underscores {
            label.replace(' ', "_")
        } else {
            label.clone()
        };
        let is_longer = match best {
            Some((_, length)) => candidate.len() > length,
            None => true,
        };
        if !candidate.is_empty() && input.starts_with(&candidate) && is_longer {
            best = Some((index, candidate.len()));
        }
    }
    best
}

fn parse_signed_step(text: &str, position: &mut usize) -> Result<isize> {
    let start = *position;
    if *position < text.len()
        && (text[*position..].starts_with('+') || text[*position..].starts_with('-'))
    {
        *position += 1;
    }
    let digits = *position;
    while *position < text.len() && text.as_bytes()[*position].is_ascii_digit() {
        *position += 1;
    }
    if *position == digits {
        return Err(Error::parse("selector stride is not an integer"));
    }
    let step = text[start..*position]
        .parse::<isize>()
        .map_err(|_| Error::parse("selector stride is outside the supported range"))?;
    if step == 0 {
        return Err(Error::invalid("selector stride cannot be zero"));
    }
    Ok(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_spec_separates_only_a_terminal_selector() {
        let spec = DatasetSpec::parse("epi+orig[2..$]").unwrap();
        assert_eq!(spec.path(), Path::new("epi+orig"));
        assert_eq!(spec.selector().unwrap().expression(), "2..$");
        assert_eq!(spec.to_string(), "epi+orig[2..$]");
        assert_eq!(
            DatasetSpec::parse("odd[name").unwrap().path(),
            Path::new("odd[name")
        );
        assert!(DatasetSpec::parse("epi+orig[]").is_err());
    }

    #[test]
    fn resolves_afni_numeric_ranges_order_and_duplicates() {
        let selector = SubBrickSelector::parse("2,7..4,1..$(3),2").unwrap();
        assert_eq!(
            selector.resolve(8, &[]).unwrap(),
            [2, 7, 6, 5, 4, 1, 4, 7, 2]
        );
        assert_eq!(
            SubBrickSelector::parse("7..1(-2)")
                .unwrap()
                .resolve(8, &[])
                .unwrap(),
            [7, 5, 3, 1]
        );
    }

    #[test]
    fn resolves_longest_labels_and_space_aliases() {
        let labels = vec!["beta".into(), "beta_Coef".into(), "visual cortex".into()];
        assert_eq!(
            SubBrickSelector::parse("beta_Coef,visual_cortex,beta")
                .unwrap()
                .resolve(3, &labels)
                .unwrap(),
            [1, 2, 0]
        );
    }

    #[test]
    fn rejects_bad_or_out_of_range_selectors() {
        assert!(SubBrickSelector::parse("1,")
            .unwrap()
            .resolve(3, &[])
            .is_err());
        assert!(SubBrickSelector::parse("0..2(0)")
            .unwrap()
            .resolve(3, &[])
            .is_err());
        assert!(SubBrickSelector::parse("0..2(-1)")
            .unwrap()
            .resolve(3, &[])
            .is_err());
        assert!(SubBrickSelector::parse("3")
            .unwrap()
            .resolve(3, &[])
            .is_err());
    }
}
