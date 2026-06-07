//! Extract the Widevine PSSH box from an encrypted fMP4 file.
//! Ported verbatim from `mattpetters/udemy-dl` `src/pssh.rs`.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

/// Widevine system ID bytes (big-endian UUID).
const WIDEVINE_UUID: [u8; 16] = [
    0xed, 0xef, 0x8b, 0xa9, 0x79, 0xd6, 0x4a, 0xce, 0xa3, 0xc8, 0x27, 0xdc, 0xd5, 0x1d, 0x21, 0xed,
];

/// Scan a buffer for the Widevine PSSH box and return it base64-encoded.
///
/// The PSSH box layout in fMP4:
///   [4 bytes: box size BE] [4 bytes: "pssh"] [4 bytes: version+flags]
///   [16 bytes: system UUID] [...]
/// We find the UUID at offset 12 from the box start.
pub fn extract_pssh_b64(data: &[u8]) -> Result<String> {
    let scan_limit = data.len().min(50 * 1024);
    let haystack = &data[..scan_limit];

    // Find all occurrences of the Widevine UUID
    for i in 0..haystack.len().saturating_sub(16) {
        if haystack[i..i + 16] == WIDEVINE_UUID {
            // UUID sits at offset 12 from the box start
            if i < 12 {
                continue;
            }
            let box_start = i - 12;

            // Read 4-byte big-endian box size
            if box_start + 4 > haystack.len() {
                continue;
            }
            let size =
                u32::from_be_bytes(haystack[box_start..box_start + 4].try_into().unwrap()) as usize;

            if size < 32 || box_start + size > data.len() {
                continue;
            }

            let pssh_box = &data[box_start..box_start + size];
            return Ok(B64.encode(pssh_box));
        }
    }

    Err(anyhow!("Widevine PSSH box not found in file"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    #[test]
    fn widevine_uuid_bytes_are_correct() {
        let expected = [
            0xed, 0xef, 0x8b, 0xa9, 0x79, 0xd6, 0x4a, 0xce, 0xa3, 0xc8, 0x27, 0xdc, 0xd5, 0x1d,
            0x21, 0xed,
        ];
        assert_eq!(WIDEVINE_UUID, expected);
    }

    /// Build a minimal valid PSSH box containing the Widevine UUID.
    fn make_pssh_box(payload: &[u8]) -> Vec<u8> {
        let size = 28u32 + payload.len() as u32;
        let mut box_ = Vec::with_capacity(size as usize);
        box_.extend_from_slice(&size.to_be_bytes());
        box_.extend_from_slice(b"pssh");
        box_.extend_from_slice(&[0u8; 4]); // version + flags
        box_.extend_from_slice(&WIDEVINE_UUID);
        box_.extend_from_slice(payload);
        box_
    }

    #[test]
    fn extracts_pssh_from_bare_box() {
        let payload = b"test-payload";
        let box_ = make_pssh_box(payload);
        let b64 = extract_pssh_b64(&box_).unwrap();
        let decoded = B64.decode(&b64).unwrap();
        assert_eq!(decoded, box_);
    }

    #[test]
    fn extracts_pssh_when_preceded_by_garbage() {
        let mut data = vec![0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];
        data.extend(make_pssh_box(b"hello"));
        data.extend(b"trailing garbage");
        let b64 = extract_pssh_b64(&data).unwrap();
        let decoded = B64.decode(&b64).unwrap();
        assert!(decoded.windows(16).any(|w| w == WIDEVINE_UUID));
    }

    #[test]
    fn returns_error_when_no_pssh_present() {
        let data = vec![0u8; 200];
        assert!(extract_pssh_b64(&data).is_err());
    }

    #[test]
    fn returns_error_on_empty_input() {
        assert!(extract_pssh_b64(&[]).is_err());
    }
}
