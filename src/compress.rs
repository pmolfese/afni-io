//! gzip / zlib / deflate helpers, backed by `flate2`.
//!
//! Used for `.nii.gz` volumes and GIfTI `GZipBase64Binary` data arrays.

use std::io::Read;

use crate::error::{Error, Result};

/// Whether `bytes` starts with the gzip magic number (`1f 8b`).
pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Decompress a gzip stream.
pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| Error::parse(format!("gzip decompression failed: {e}")))?;
    Ok(out)
}

/// Compress `bytes` to a gzip stream at the default level.
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to an in-memory Vec is infallible.
    enc.write_all(bytes).expect("gzip into Vec");
    enc.finish().expect("gzip finish")
}

/// Decompress data that may be gzip, zlib, or raw deflate.
///
/// GIfTI files labelled `GZipBase64Binary` are sometimes actually zlib or raw
/// deflate streams, so each form is tried in turn.
pub fn inflate_flexible(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .is_ok()
    {
        return Ok(out);
    }
    out.clear();
    if flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut out)
        .is_ok()
    {
        return Ok(out);
    }
    out.clear();
    flate2::read::DeflateDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| Error::parse(format!("deflate decompression failed: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_round_trips() {
        let data = b"the quick brown fox".repeat(100);
        let packed = gzip(&data);
        assert!(is_gzip(&packed));
        assert_eq!(gunzip(&packed).unwrap(), data);
        assert_eq!(inflate_flexible(&packed).unwrap(), data);
    }
}
