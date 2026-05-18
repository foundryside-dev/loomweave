//! Core-owned pre-ingest secret scanner.
//!
//! The scanner intentionally stores only positions, rule identifiers, and a
//! detect-secrets-compatible SHA-1 digest of the matched bytes. Literal secret
//! values do not leave the scanning call.

mod baseline;
mod entropy;
mod patterns;

pub use baseline::{
    Baseline, BaselineEntry, BaselineError, BaselineMatch, SuppressionResult, load_baseline,
};
pub use entropy::EntropyTuning;
pub use patterns::{PatternMeta, Scanner};

/// One secret-like match in one file buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub rule_id: &'static str,
    pub category: SecretCategory,
    pub byte_offset: usize,
    pub line_number: u32,
    pub matched_len: usize,
    pub hashed_secret: [u8; 20],
}

/// High-level category used for evidence and future operator grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretCategory {
    CloudCredential,
    VcsCredential,
    AiProviderCredential,
    PaymentsCredential,
    MessagingCredential,
    PrivateKey,
    JwtToken,
    HighEntropy,
    ContextualCredential,
}

fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    use sha1::{Digest, Sha1};

    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn line_number_for_offset(buf: &[u8], offset: usize) -> u32 {
    let line = buf
        .get(..offset.min(buf.len()))
        .unwrap_or(buf)
        .iter()
        .fold(0usize, |count, byte| count + usize::from(*byte == b'\n'))
        + 1;
    u32::try_from(line).unwrap_or(u32::MAX)
}

fn hex_encode(bytes: &[u8; 20]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn hex_decode_20(input: &str) -> Result<[u8; 20], String> {
    let raw = input.trim();
    if raw.len() != 40 {
        return Err(format!("expected 40 hex characters, got {}", raw.len()));
    }
    let mut out = [0u8; 20];
    for (idx, chunk) in raw.as_bytes().chunks_exact(2).enumerate() {
        let hi = hex_value(chunk[0]).ok_or_else(|| format!("invalid hex at byte {}", idx * 2))?;
        let lo =
            hex_value(chunk[1]).ok_or_else(|| format!("invalid hex at byte {}", idx * 2 + 1))?;
        out[idx] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
