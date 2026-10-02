//! AFNI `.1D` text tables.
//!
//! A `.1D` file is a whitespace-separated numeric matrix: one row per line,
//! columns separated by spaces or tabs. Lines beginning with `#` are comments,
//! and a trailing `#`-comment on a data line is ignored. Every data row is
//! expected to have the same number of columns.
//!
//! This is AFNI's lingua-franca for time series, regressors, motion parameters,
//! and node value lists.
//!
//! Reference: `afni/src/matlab/Read_1D.m`, `afni/src/thd_1Ddset.c`.

use std::path::Path;

use crate::error::{self, from_utf8, Error, Result};

/// A parsed `.1D` numeric table, stored row-major.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OneD {
    /// Number of rows.
    pub rows: usize,
    /// Number of columns.
    pub cols: usize,
    /// `rows * cols` values, row-major.
    pub values: Vec<f64>,
    /// Comment lines encountered (without the leading `#`), in file order.
    pub comments: Vec<String>,
}

impl OneD {
    /// Read a `.1D` file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = error::read_file(path.as_ref())?;
        Self::parse(&from_utf8(&bytes, "1D file")?)
    }

    /// Parse `.1D` text.
    pub fn parse(text: &str) -> Result<Self> {
        let mut values = Vec::new();
        let mut comments = Vec::new();
        let mut cols: Option<usize> = None;
        let mut rows = 0;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('#') {
                comments.push(rest.trim().to_string());
                continue;
            }
            // Drop any inline trailing comment.
            let data = line.split('#').next().unwrap_or("").trim();
            if data.is_empty() {
                continue;
            }
            let row: Vec<f64> = data
                .split_whitespace()
                .map(|t| {
                    t.parse::<f64>()
                        .map_err(|_| Error::parse(format!("1D: invalid value {t:?}")))
                })
                .collect::<Result<_>>()?;
            match cols {
                None => cols = Some(row.len()),
                Some(expected) if expected != row.len() => {
                    return Err(Error::parse(format!(
                        "1D: row {} has {} columns, expected {expected}",
                        rows + 1,
                        row.len()
                    )));
                }
                _ => {}
            }
            values.extend(row);
            rows += 1;
        }

        Ok(Self {
            rows,
            cols: cols.unwrap_or(0),
            values,
            comments,
        })
    }

    /// Fetch the value at `(row, col)`, if in bounds.
    pub fn get(&self, row: usize, col: usize) -> Option<f64> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.values.get(row * self.cols + col).copied()
    }

    /// Extract a single column as a contiguous vector.
    pub fn column(&self, col: usize) -> Option<Vec<f64>> {
        if col >= self.cols {
            return None;
        }
        Some(
            (0..self.rows)
                .map(|r| self.values[r * self.cols + col])
                .collect(),
        )
    }

    /// Build a table from a vector of equal-length rows.
    pub fn from_rows(rows: Vec<Vec<f64>>) -> Result<Self> {
        let cols = rows.first().map(Vec::len).unwrap_or(0);
        let mut values = Vec::with_capacity(rows.len() * cols);
        for (i, row) in rows.iter().enumerate() {
            if row.len() != cols {
                return Err(Error::invalid(format!(
                    "1D: row {i} has {} columns, expected {cols}",
                    row.len()
                )));
            }
            values.extend_from_slice(row);
        }
        Ok(Self {
            rows: rows.len(),
            cols,
            values,
            comments: Vec::new(),
        })
    }

    /// Serialise to `.1D` text (comments first, then the matrix).
    pub fn to_1d_string(&self) -> String {
        let mut out = String::new();
        for comment in &self.comments {
            out.push_str("# ");
            out.push_str(comment);
            out.push('\n');
        }
        for r in 0..self.rows {
            for c in 0..self.cols {
                if c > 0 {
                    out.push(' ');
                }
                out.push_str(&crate::niml::format_float(self.values[r * self.cols + c]));
            }
            out.push('\n');
        }
        out
    }

    /// Write to a `.1D` file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_1d_string().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_matrix_with_comments() {
        let text = "# motion params\n0.1 0.2 0.3\n0.4 0.5 0.6  # second TR\n";
        let one = OneD::parse(text).unwrap();
        assert_eq!(one.rows, 2);
        assert_eq!(one.cols, 3);
        assert_eq!(one.get(1, 2), Some(0.6));
        assert_eq!(one.column(0), Some(vec![0.1, 0.4]));
        assert_eq!(one.comments, vec!["motion params"]);
    }

    #[test]
    fn rejects_ragged_rows() {
        assert!(OneD::parse("1 2 3\n4 5\n").is_err());
    }

    #[test]
    fn round_trips() {
        let one = OneD::from_rows(vec![vec![1.0, 2.0], vec![3.0, 4.0]]).unwrap();
        let reparsed = OneD::parse(&one.to_1d_string()).unwrap();
        assert_eq!(one.values, reparsed.values);
    }
}
