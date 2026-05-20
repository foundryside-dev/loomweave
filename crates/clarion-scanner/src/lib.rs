//! Core-owned pre-ingest secret scanner.
//!
//! The scanner intentionally stores only positions, rule identifiers, and a
//! detect-secrets-compatible SHA-1 digest of the matched bytes. Literal secret
//! values do not leave the scanning call.

mod baseline;
mod entropy;
mod patterns;

pub use baseline::{
    Baseline, BaselineEntry, BaselineEntryIssue, BaselineError, BaselineMatch, SuppressionResult,
    load_baseline,
};
pub use entropy::EntropyTuning;
pub use patterns::{PatternMeta, Scanner};
use std::{
    fmt::{self, Write as _},
    str::FromStr,
};

/// One secret-like match in one file buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub rule_id: &'static str,
    pub detect_secrets_type: DetectSecretsRule,
    pub category: SecretCategory,
    pub byte_offset: usize,
    pub line_number: u32,
    pub matched_len: usize,
    pub hashed_secret: HashedSecret,
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

/// detect-secrets-compatible SHA-1 digest of the matched secret bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HashedSecret([u8; 20]);

impl HashedSecret {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    pub fn from_hex(input: &str) -> Result<Self, HexDigestError> {
        let raw = input.trim();
        if raw.len() != 40 {
            return Err(HexDigestError {
                message: format!("expected 40 hex characters, got {}", raw.len()),
            });
        }
        let mut out = [0u8; 20];
        for (idx, chunk) in raw.as_bytes().chunks_exact(2).enumerate() {
            let hi = hex_value(chunk[0]).ok_or_else(|| HexDigestError {
                message: format!("invalid hex at byte {}", idx * 2),
            })?;
            let lo = hex_value(chunk[1]).ok_or_else(|| HexDigestError {
                message: format!("invalid hex at byte {}", idx * 2 + 1),
            })?;
            out[idx] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

impl fmt::Display for HashedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in self.0 {
            f.write_char(char::from(HEX[usize::from(byte >> 4)]))?;
            f.write_char(char::from(HEX[usize::from(byte & 0x0f)]))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HexDigestError {
    message: String,
}

/// Closed detector type vocabulary supported by Clarion's v0.1 scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DetectSecretsRule {
    AwsAccessKey,
    AwsSecretAccessKey,
    GitHubToken,
    GitHubFineGrainedToken,
    GitHubOAuthToken,
    AnthropicApiKey,
    OpenAiApiKey,
    StripeApiKey,
    SlackToken,
    JwtToken,
    PrivateKey,
    KeywordDetector,
    Base64HighEntropyString,
    HexHighEntropyString,
}

impl DetectSecretsRule {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwsAccessKey => "AWS Access Key",
            Self::AwsSecretAccessKey => "AWS Secret Access Key",
            Self::GitHubToken => "GitHub Token",
            Self::GitHubFineGrainedToken => "GitHub Fine-Grained Token",
            Self::GitHubOAuthToken => "GitHub OAuth Token",
            Self::AnthropicApiKey => "Anthropic API Key",
            Self::OpenAiApiKey => "OpenAI API Key",
            Self::StripeApiKey => "Stripe API Key",
            Self::SlackToken => "Slack Token",
            Self::JwtToken => "JWT Token",
            Self::PrivateKey => "Private Key",
            Self::KeywordDetector => "Keyword Detector",
            Self::Base64HighEntropyString => "Base64 High Entropy String",
            Self::HexHighEntropyString => "Hex High Entropy String",
        }
    }

    #[must_use]
    pub fn rule_id(self) -> &'static str {
        match self {
            Self::AwsAccessKey => "AwsAccessKeyId",
            Self::AwsSecretAccessKey => "AwsSecretAccessKey",
            Self::GitHubToken => "GitHubPat",
            Self::GitHubFineGrainedToken => "GitHubFineGrainedPat",
            Self::GitHubOAuthToken => "GitHubOAuth",
            Self::AnthropicApiKey => "AnthropicApiKey",
            Self::OpenAiApiKey => "OpenAiApiKey",
            Self::StripeApiKey => "StripeApiKey",
            Self::SlackToken => "SlackToken",
            Self::JwtToken => "JwtToken",
            Self::PrivateKey => "PrivateKeyHeader",
            Self::KeywordDetector => "ContextualCredential",
            Self::Base64HighEntropyString => "HighEntropyBase64",
            Self::HexHighEntropyString => "HighEntropyHex",
        }
    }
}

impl fmt::Display for DetectSecretsRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DetectSecretsRule {
    type Err = UnknownDetectSecretsRule;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "AWS Access Key" => Ok(Self::AwsAccessKey),
            "AWS Secret Access Key" => Ok(Self::AwsSecretAccessKey),
            "GitHub Token" => Ok(Self::GitHubToken),
            "GitHub Fine-Grained Token" => Ok(Self::GitHubFineGrainedToken),
            "GitHub OAuth Token" => Ok(Self::GitHubOAuthToken),
            "Anthropic API Key" => Ok(Self::AnthropicApiKey),
            "OpenAI API Key" => Ok(Self::OpenAiApiKey),
            "Stripe API Key" => Ok(Self::StripeApiKey),
            "Slack Token" => Ok(Self::SlackToken),
            "JWT Token" => Ok(Self::JwtToken),
            "Private Key" => Ok(Self::PrivateKey),
            "Keyword Detector" => Ok(Self::KeywordDetector),
            "Base64 High Entropy String" => Ok(Self::Base64HighEntropyString),
            "Hex High Entropy String" => Ok(Self::HexHighEntropyString),
            _ => Err(UnknownDetectSecretsRule {
                value: value.to_owned(),
            }),
        }
    }
}

impl TryFrom<&str> for DetectSecretsRule {
    type Error = UnknownDetectSecretsRule;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::from_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported detector type {value:?}")]
pub struct UnknownDetectSecretsRule {
    value: String,
}

fn sha1_digest(bytes: &[u8]) -> HashedSecret {
    use sha1::{Digest, Sha1};

    let mut hasher = Sha1::new();
    hasher.update(bytes);
    HashedSecret(hasher.finalize().into())
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

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
