//! Self-check of Pilatus miniCBF frames. The MIME header of a frame's binary section
//! carries `Content-MD5` (base64 MD5 of the `X-Binary-Size` compressed image bytes that
//! follow the `0C 1A 04 D5` marker), so a frame can tell by itself whether it arrived,
//! or sits on disk, intact: no extra request to the server.

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::path::Path;

/// Start of the binary image data, right after the MIME header.
const MARK: &[u8] = b"\x0c\x1a\x04\xd5";
/// Opens the MIME header of a binary section.
const BOUNDARY: &[u8] = b"--CIF-BINARY-FORMAT-SECTION--";

/// What the check found (a mismatch is the error [`Corrupt`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The binary section matches its `Content-MD5`.
    Verified,
    /// Nothing to check: not a frame, no `Content-MD5`, or a cloud-only placeholder.
    Unchecked,
}

/// A frame whose bytes don't match its own header: fetch it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corrupt(pub String);

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt CBF: {}", self.0)
    }
}

impl std::error::Error for Corrupt {}

/// Frames are `.cbf` files; only those are checked.
pub fn is_cbf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cbf"))
}

/// Checks a CBF held in memory against its own `Content-MD5`.
pub fn check(data: &[u8]) -> Result<Check, Corrupt> {
    let Some(at) = find(data, MARK) else {
        // a header that promises a checksum but lost its binary section is corrupt
        return match header_value(data, "Content-MD5:") {
            Some(_) => Err(Corrupt("Content-MD5 present but no binary section".into())),
            None => Ok(Check::Unchecked),
        };
    };
    let head = &data[..at];
    let mime = rfind(head, BOUNDARY).map_or(head, |i| &head[i..]);
    let Some(want) = header_value(mime, "Content-MD5:") else {
        return Ok(Check::Unchecked);
    };
    let size: usize = header_value(mime, "X-Binary-Size:")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Corrupt("Content-MD5 without a valid X-Binary-Size".into()))?;
    let start = at + MARK.len();
    let Some(binary) = start.checked_add(size).and_then(|end| data.get(start..end)) else {
        return Err(Corrupt(format!(
            "binary section cut short: {} of {size} bytes",
            data.len() - start
        )));
    };
    let got = STANDARD.encode(md5::compute(binary).0);
    if got.trim_end_matches('=') == want.trim_end_matches('=') {
        Ok(Check::Verified)
    } else {
        Err(Corrupt(format!("Content-MD5 mismatch (header {want}, data {got})")))
    }
}

/// Checks a file on disk, whatever its name ([`is_cbf`] decides whether to call this).
/// A mismatch is an error that downcasts to [`Corrupt`]. A cloud-only placeholder
/// (OneDrive Files On-Demand) is not read, which would download it: `Unchecked`.
pub fn check_file(path: &Path) -> Result<Check> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if cloud_only(&meta) {
        return Ok(Check::Unchecked);
    }
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(check(&data)?)
}

#[cfg(windows)]
fn cloud_only(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x0000_1000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    meta.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(windows))]
fn cloud_only(_meta: &std::fs::Metadata) -> bool {
    false
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// The value of the first `key` header line (key matched case-insensitively, as MIME does).
fn header_value(head: &[u8], key: &str) -> Option<String> {
    head.split(|&b| b == b'\n').find_map(|line| {
        let line = line.trim_ascii();
        let (k, v) = line.split_at_checked(key.len())?;
        k.eq_ignore_ascii_case(key.as_bytes())
            .then(|| String::from_utf8_lossy(v.trim_ascii()).into_owned())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Byte-offset-style payload; ends with the marker itself, which must not confuse the parser.
    const BINARY: &[u8] = &[
        0x05, 0x80, 0x00, 0x01, 0x02, 0xfe, 0x80, 0x00, 0x80, 0x10, 0x27, 0x00, 0x00, 0x7f, 0x0c, 0x1a, 0x04, 0xd5,
    ];
    /// MD5 of `BINARY`, worked out independently (Python `hashlib` + `base64`).
    pub(crate) const BINARY_MD5: &str = "xtzo6j08uRJGPsthNRmbxw==";

    /// A tiny miniCBF as the Pilatus writes it (CRLF, padding after the binary section).
    pub(crate) fn tiny_cbf(md5: Option<&str>) -> Vec<u8> {
        let md5_line = md5.map(|m| format!("Content-MD5: {m}\r\n")).unwrap_or_default();
        let head = format!(
            "###CBF: VERSION 1.5, CBFlib v0.7.8 - PILATUS detectors\r\n\r\ndata_tiny\r\n\r\n\
             _array_data.header_convention \"PILATUS_1.2\"\r\n_array_data.header_contents\r\n;\r\n\
             # Detector: PILATUS 6M, S/N 60-0108\r\n# Start_angle 0.0000 deg.\r\n;\r\n\r\n\
             _array_data.data\r\n;\r\n--CIF-BINARY-FORMAT-SECTION--\r\n\
             Content-Type: application/octet-stream;\r\n     conversions=\"x-CBF_BYTE_OFFSET\"\r\n\
             Content-Transfer-Encoding: BINARY\r\nX-Binary-Size: {}\r\nX-Binary-ID: 1\r\n\
             X-Binary-Element-Type: \"signed 32-bit integer\"\r\nX-Binary-Element-Byte-Order: LITTLE_ENDIAN\r\n\
             {md5_line}X-Binary-Number-of-Elements: 6\r\nX-Binary-Size-Fastest-Dimension: 3\r\n\
             X-Binary-Size-Second-Dimension: 2\r\nX-Binary-Size-Padding: 8\r\n\r\n",
            BINARY.len()
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(MARK);
        out.extend_from_slice(BINARY);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(b"\r\n--CIF-BINARY-FORMAT-SECTION----\r\n;\r\n");
        out
    }

    fn binary_start(cbf: &[u8]) -> usize {
        find(cbf, MARK).unwrap() + MARK.len()
    }

    #[test]
    fn md5_of_known_bytes() {
        assert_eq!(STANDARD.encode(md5::compute(b"").0), "1B2M2Y8AsgTpgAmY7PhCfg==");
        assert_eq!(STANDARD.encode(md5::compute(BINARY).0), BINARY_MD5);
    }

    #[test]
    fn correct_md5_passes() {
        assert_eq!(check(&tiny_cbf(Some(BINARY_MD5))), Ok(Check::Verified));
        // unpadded base64 is accepted too
        assert_eq!(check(&tiny_cbf(Some(BINARY_MD5.trim_end_matches('=')))), Ok(Check::Verified));
    }

    #[test]
    fn a_flipped_byte_in_the_binary_section_fails() {
        let good = tiny_cbf(Some(BINARY_MD5));
        let start = binary_start(&good);
        for i in 0..BINARY.len() {
            let mut bad = good.clone();
            bad[start + i] ^= 0x01;
            let err = check(&bad).expect_err("flipped byte must fail");
            assert!(err.0.contains("mismatch"), "{err}");
        }
    }

    #[test]
    fn only_x_binary_size_bytes_are_hashed() {
        let mut cbf = tiny_cbf(Some(BINARY_MD5));
        let padding = binary_start(&cbf) + BINARY.len();
        cbf[padding] = 0xff;
        assert_eq!(check(&cbf), Ok(Check::Verified));
    }

    #[test]
    fn a_truncated_frame_fails() {
        let cbf = tiny_cbf(Some(BINARY_MD5));
        let cut = &cbf[..binary_start(&cbf) + BINARY.len() - 1];
        let err = check(cut).expect_err("truncated must fail");
        assert!(err.0.contains("cut short"), "{err}");
        // cut inside the header, after the checksum line: the binary section is gone
        let head = &cbf[..binary_start(&cbf) - MARK.len() - 2];
        assert!(check(head).is_err());
    }

    #[test]
    fn a_wrong_checksum_in_the_header_fails() {
        assert!(check(&tiny_cbf(Some("1B2M2Y8AsgTpgAmY7PhCfg=="))).is_err());
    }

    #[test]
    fn a_cbf_without_content_md5_passes_unchecked() {
        assert_eq!(check(&tiny_cbf(None)), Ok(Check::Unchecked));
    }

    #[test]
    fn a_non_cbf_file_passes_unchecked() {
        let hkl = b"   0   0   2   12.34    0.56\r\n   0   0   0    0.00    0.00\r\n";
        assert_eq!(check(hkl), Ok(Check::Unchecked));
        assert_eq!(check(b""), Ok(Check::Unchecked));
        assert!(is_cbf(Path::new("frames_r1/JSG331_1_0001.cbf")));
        assert!(is_cbf(Path::new("X_0001.CBF")));
        assert!(!is_cbf(Path::new("XDS_ASCII.HKL")));
        assert!(!is_cbf(Path::new("JSG331_1_0001.cbf.part")));
    }

    #[test]
    fn check_file_reads_from_disk() {
        let dir = std::env::temp_dir().join(format!("amanece_cbf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.cbf");
        std::fs::write(&good, tiny_cbf(Some(BINARY_MD5))).unwrap();
        assert_eq!(check_file(&good).unwrap(), Check::Verified);
        let mut bytes = tiny_cbf(Some(BINARY_MD5));
        let start = binary_start(&bytes);
        bytes[start + 3] ^= 0x80;
        let bad = dir.join("bad.cbf");
        std::fs::write(&bad, bytes).unwrap();
        assert!(check_file(&bad).unwrap_err().is::<Corrupt>());
        assert!(!check_file(&dir.join("missing.cbf")).unwrap_err().is::<Corrupt>());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Real frames, by hand: `AMANECE_CBF_DIR=<folder of .cbf> cargo test real_frames -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_frames() {
        let dir = std::env::var("AMANECE_CBF_DIR").expect("set AMANECE_CBF_DIR");
        let (mut verified, mut unchecked) = (0, 0);
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !is_cbf(&path) {
                continue;
            }
            match check_file(&path) {
                Ok(Check::Verified) => verified += 1,
                Ok(Check::Unchecked) => unchecked += 1,
                Err(e) => panic!("{}: {e:#}", path.display()),
            }
        }
        println!("{verified} verified, {unchecked} unchecked");
        assert!(verified > 0);
    }
}
