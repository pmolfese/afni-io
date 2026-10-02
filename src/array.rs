//! Shared typed numeric arrays for NIfTI and GIfTI.
//!
//! Both formats label their data with the same NIfTI datatype codes (from
//! `nifti1.h`) and store it either as ASCII text or as little/big-endian binary.
//! This module owns that common ground: the [`DataType`] code table, a typed
//! [`TypedArray`] buffer, and the encode/decode routines used by both
//! [`crate::nifti`] and [`crate::gifti`].

use crate::error::{Error, Result};

/// A scalar NIfTI datatype. The discriminant matches the `nifti1.h` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// `NIFTI_TYPE_UINT8` (2).
    UInt8 = 2,
    /// `NIFTI_TYPE_INT16` (4).
    Int16 = 4,
    /// `NIFTI_TYPE_INT32` (8).
    Int32 = 8,
    /// `NIFTI_TYPE_FLOAT32` (16).
    Float32 = 16,
    /// `NIFTI_TYPE_FLOAT64` (64).
    Float64 = 64,
    /// `NIFTI_TYPE_INT8` (256).
    Int8 = 256,
    /// `NIFTI_TYPE_UINT16` (512).
    UInt16 = 512,
    /// `NIFTI_TYPE_UINT32` (768).
    UInt32 = 768,
    /// `NIFTI_TYPE_INT64` (1024).
    Int64 = 1024,
    /// `NIFTI_TYPE_UINT64` (1280).
    UInt64 = 1280,
}

impl DataType {
    /// Map a NIfTI datatype integer code onto a variant.
    pub fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            2 => Self::UInt8,
            4 => Self::Int16,
            8 => Self::Int32,
            16 => Self::Float32,
            64 => Self::Float64,
            256 => Self::Int8,
            512 => Self::UInt16,
            768 => Self::UInt32,
            1024 => Self::Int64,
            1280 => Self::UInt64,
            _ => return None,
        })
    }

    /// Map a `NIFTI_TYPE_*` name onto a variant.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "NIFTI_TYPE_UINT8" => Self::UInt8,
            "NIFTI_TYPE_INT16" => Self::Int16,
            "NIFTI_TYPE_INT32" => Self::Int32,
            "NIFTI_TYPE_FLOAT32" => Self::Float32,
            "NIFTI_TYPE_FLOAT64" => Self::Float64,
            "NIFTI_TYPE_INT8" => Self::Int8,
            "NIFTI_TYPE_UINT16" => Self::UInt16,
            "NIFTI_TYPE_UINT32" => Self::UInt32,
            "NIFTI_TYPE_INT64" => Self::Int64,
            "NIFTI_TYPE_UINT64" => Self::UInt64,
            _ => return None,
        })
    }

    /// The canonical `NIFTI_TYPE_*` name.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::UInt8 => "NIFTI_TYPE_UINT8",
            Self::Int16 => "NIFTI_TYPE_INT16",
            Self::Int32 => "NIFTI_TYPE_INT32",
            Self::Float32 => "NIFTI_TYPE_FLOAT32",
            Self::Float64 => "NIFTI_TYPE_FLOAT64",
            Self::Int8 => "NIFTI_TYPE_INT8",
            Self::UInt16 => "NIFTI_TYPE_UINT16",
            Self::UInt32 => "NIFTI_TYPE_UINT32",
            Self::Int64 => "NIFTI_TYPE_INT64",
            Self::UInt64 => "NIFTI_TYPE_UINT64",
        }
    }

    /// The integer datatype code.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// The number of bits per element (`bitpix`).
    pub fn bitpix(self) -> i16 {
        (self.elem_size() * 8) as i16
    }

    /// The number of bytes per element.
    pub fn elem_size(self) -> usize {
        match self {
            Self::UInt8 | Self::Int8 => 1,
            Self::Int16 | Self::UInt16 => 2,
            Self::Int32 | Self::UInt32 | Self::Float32 => 4,
            Self::Int64 | Self::UInt64 | Self::Float64 => 8,
        }
    }
}

/// A typed numeric buffer; the variant matches its [`DataType`].
#[derive(Debug, Clone, PartialEq)]
pub enum TypedArray {
    /// 8-bit unsigned.
    UInt8(Vec<u8>),
    /// 8-bit signed.
    Int8(Vec<i8>),
    /// 16-bit unsigned.
    UInt16(Vec<u16>),
    /// 16-bit signed.
    Int16(Vec<i16>),
    /// 32-bit unsigned.
    UInt32(Vec<u32>),
    /// 32-bit signed.
    Int32(Vec<i32>),
    /// 64-bit unsigned.
    UInt64(Vec<u64>),
    /// 64-bit signed.
    Int64(Vec<i64>),
    /// 32-bit float.
    Float32(Vec<f32>),
    /// 64-bit float.
    Float64(Vec<f64>),
}

impl TypedArray {
    /// The datatype of this buffer.
    pub fn dtype(&self) -> DataType {
        match self {
            Self::UInt8(_) => DataType::UInt8,
            Self::Int8(_) => DataType::Int8,
            Self::UInt16(_) => DataType::UInt16,
            Self::Int16(_) => DataType::Int16,
            Self::UInt32(_) => DataType::UInt32,
            Self::Int32(_) => DataType::Int32,
            Self::UInt64(_) => DataType::UInt64,
            Self::Int64(_) => DataType::Int64,
            Self::Float32(_) => DataType::Float32,
            Self::Float64(_) => DataType::Float64,
        }
    }

    /// The number of elements.
    pub fn len(&self) -> usize {
        match self {
            Self::UInt8(v) => v.len(),
            Self::Int8(v) => v.len(),
            Self::UInt16(v) => v.len(),
            Self::Int16(v) => v.len(),
            Self::UInt32(v) => v.len(),
            Self::Int32(v) => v.len(),
            Self::UInt64(v) => v.len(),
            Self::Int64(v) => v.len(),
            Self::Float32(v) => v.len(),
            Self::Float64(v) => v.len(),
        }
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element at `index`, widened to `f64`, if in bounds.
    pub fn get_f64(&self, index: usize) -> Option<f64> {
        Some(match self {
            Self::UInt8(v) => *v.get(index)? as f64,
            Self::Int8(v) => *v.get(index)? as f64,
            Self::UInt16(v) => *v.get(index)? as f64,
            Self::Int16(v) => *v.get(index)? as f64,
            Self::UInt32(v) => *v.get(index)? as f64,
            Self::Int32(v) => *v.get(index)? as f64,
            Self::UInt64(v) => *v.get(index)? as f64,
            Self::Int64(v) => *v.get(index)? as f64,
            Self::Float32(v) => *v.get(index)? as f64,
            Self::Float64(v) => *v.get(index)?,
        })
    }

    /// Collect the whole buffer as `f64` values.
    pub fn to_f64_vec(&self) -> Vec<f64> {
        (0..self.len()).map(|i| self.get_f64(i).unwrap()).collect()
    }

    /// Encode the buffer to bytes in the requested byte order.
    pub fn to_bytes(&self, little: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len() * self.dtype().elem_size());
        macro_rules! push {
            ($v:expr) => {
                for x in $v {
                    if little {
                        out.extend_from_slice(&x.to_le_bytes());
                    } else {
                        out.extend_from_slice(&x.to_be_bytes());
                    }
                }
            };
        }
        match self {
            Self::UInt8(v) => out.extend_from_slice(v),
            Self::Int8(v) => out.extend(v.iter().map(|&x| x as u8)),
            Self::UInt16(v) => push!(v),
            Self::Int16(v) => push!(v),
            Self::UInt32(v) => push!(v),
            Self::Int32(v) => push!(v),
            Self::UInt64(v) => push!(v),
            Self::Int64(v) => push!(v),
            Self::Float32(v) => push!(v),
            Self::Float64(v) => push!(v),
        }
        out
    }

    /// Render the buffer as a space-separated ASCII string.
    pub fn to_ascii(&self) -> String {
        fn join<T: std::fmt::Display>(v: &[T]) -> String {
            v.iter().map(T::to_string).collect::<Vec<_>>().join(" ")
        }
        match self {
            Self::UInt8(v) => join(v),
            Self::Int8(v) => join(v),
            Self::UInt16(v) => join(v),
            Self::Int16(v) => join(v),
            Self::UInt32(v) => join(v),
            Self::Int32(v) => join(v),
            Self::UInt64(v) => join(v),
            Self::Int64(v) => join(v),
            Self::Float32(v) => v
                .iter()
                .map(|x| crate::niml::format_float(*x as f64))
                .collect::<Vec<_>>()
                .join(" "),
            Self::Float64(v) => v
                .iter()
                .map(|x| crate::niml::format_float(*x))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// Decode binary bytes into a [`TypedArray`].
///
/// If `expected` is non-zero it is checked against the element count.
pub fn decode_binary(
    bytes: &[u8],
    dtype: DataType,
    little: bool,
    expected: usize,
) -> Result<TypedArray> {
    let elem = dtype.elem_size();
    if bytes.len() % elem != 0 {
        return Err(Error::parse(format!(
            "binary array has {} bytes, not a multiple of {elem}",
            bytes.len()
        )));
    }
    let count = bytes.len() / elem;
    if expected != 0 && count != expected {
        return Err(Error::parse(format!(
            "binary array has {count} elements, expected {expected}"
        )));
    }

    macro_rules! decode {
        ($ty:ty, $size:literal) => {{
            let mut out: Vec<$ty> = Vec::with_capacity(count);
            for chunk in bytes.chunks_exact($size) {
                let arr: [u8; $size] = chunk.try_into().unwrap();
                out.push(if little {
                    <$ty>::from_le_bytes(arr)
                } else {
                    <$ty>::from_be_bytes(arr)
                });
            }
            out
        }};
    }

    Ok(match dtype {
        DataType::UInt8 => TypedArray::UInt8(bytes.to_vec()),
        DataType::Int8 => TypedArray::Int8(bytes.iter().map(|&b| b as i8).collect()),
        DataType::UInt16 => TypedArray::UInt16(decode!(u16, 2)),
        DataType::Int16 => TypedArray::Int16(decode!(i16, 2)),
        DataType::UInt32 => TypedArray::UInt32(decode!(u32, 4)),
        DataType::Int32 => TypedArray::Int32(decode!(i32, 4)),
        DataType::UInt64 => TypedArray::UInt64(decode!(u64, 8)),
        DataType::Int64 => TypedArray::Int64(decode!(i64, 8)),
        DataType::Float32 => TypedArray::Float32(decode!(f32, 4)),
        DataType::Float64 => TypedArray::Float64(decode!(f64, 8)),
    })
}

/// Decode whitespace-separated ASCII numbers into a [`TypedArray`].
pub fn decode_ascii(text: &str, dtype: DataType, expected: usize) -> Result<TypedArray> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if expected != 0 && tokens.len() != expected {
        return Err(Error::parse(format!(
            "ASCII array has {} tokens, expected {expected}",
            tokens.len()
        )));
    }
    macro_rules! parse {
        ($ty:ty) => {{
            let mut out: Vec<$ty> = Vec::with_capacity(tokens.len());
            for tok in &tokens {
                out.push(
                    tok.parse::<$ty>().map_err(|_| {
                        Error::parse(format!("invalid ASCII numeric token {tok:?}"))
                    })?,
                );
            }
            out
        }};
    }
    Ok(match dtype {
        DataType::UInt8 => TypedArray::UInt8(parse!(u8)),
        DataType::Int8 => TypedArray::Int8(parse!(i8)),
        DataType::UInt16 => TypedArray::UInt16(parse!(u16)),
        DataType::Int16 => TypedArray::Int16(parse!(i16)),
        DataType::UInt32 => TypedArray::UInt32(parse!(u32)),
        DataType::Int32 => TypedArray::Int32(parse!(i32)),
        DataType::UInt64 => TypedArray::UInt64(parse!(u64)),
        DataType::Int64 => TypedArray::Int64(parse!(i64)),
        DataType::Float32 => TypedArray::Float32(parse!(f32)),
        DataType::Float64 => TypedArray::Float64(parse!(f64)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_round_trips() {
        let arr = TypedArray::Float32(vec![1.5, -2.0, 3.25]);
        let bytes = arr.to_bytes(true);
        let back = decode_binary(&bytes, DataType::Float32, true, 3).unwrap();
        assert_eq!(arr, back);
    }

    #[test]
    fn ascii_round_trips_ints() {
        let arr = TypedArray::Int32(vec![0, 1, 2, 70000]);
        let back = decode_ascii(&arr.to_ascii(), DataType::Int32, 4).unwrap();
        assert_eq!(arr, back);
    }

    #[test]
    fn dtype_code_table() {
        assert_eq!(DataType::from_code(16), Some(DataType::Float32));
        assert_eq!(DataType::Float32.code(), 16);
        assert_eq!(DataType::Float32.bitpix(), 32);
        assert_eq!(
            DataType::from_name("NIFTI_TYPE_INT16"),
            Some(DataType::Int16)
        );
    }
}
