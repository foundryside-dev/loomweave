//! Embedding provider surface for `WS5b` semantic search (`search_semantic`).
//!
//! Mirrors [`crate::llm_provider`]: a small trait with a deterministic recording
//! double for tests and one live API-endpoint implementation. Embeddings are
//! **opt-in** (off by default, like the LLM policy) — Weft is local-first, so
//! nothing here makes a hosted service *required*. When semantic search is off
//! the MCP tool degrades honestly; it never fabricates an empty-as-complete
//! result.
//!
//! The trait, not the choice of provider, is load-bearing: the API-endpoint impl
//! ships first (`D-WS5b-1`), and a bundled local-model impl can be added later
//! behind the same trait.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while producing embeddings. Mirrors the live/recording split of
/// [`crate::llm_provider::LlmProviderError`].
#[derive(Debug, Error, PartialEq)]
pub enum EmbeddingProviderError {
    #[error("recording fixture has no embedding for text on model {model_id:?}")]
    MissingRecording { model_id: String },

    #[error("live embedding provider requires explicit opt-in")]
    LiveProviderNotAllowed,

    #[error("live embedding provider requires an API key")]
    MissingApiKey,

    #[error("live embedding HTTP request failed: {message}")]
    Http { message: String, retryable: bool },

    #[error("live embedding provider returned status {status}: {message}")]
    Provider {
        status: u16,
        message: String,
        retryable: bool,
    },

    #[error("invalid live embedding response: {message}")]
    InvalidResponse { message: String, retryable: bool },

    #[error("invalid embedding provider configuration: {message}")]
    InvalidConfig { message: String },
}

impl EmbeddingProviderError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::MissingRecording { .. }
            | Self::LiveProviderNotAllowed
            | Self::MissingApiKey
            | Self::InvalidConfig { .. } => false,
            Self::Http { retryable, .. }
            | Self::Provider { retryable, .. }
            | Self::InvalidResponse { retryable, .. } => *retryable,
        }
    }
}

/// A provider that turns text into dense float vectors. One `embed` call
/// processes a batch; the returned vectors are positionally aligned with the
/// input `texts` and each has length [`EmbeddingProvider::dimensions`].
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// The model identifier embeddings are keyed by (cache invalidation).
    fn model_id(&self) -> &str;
    /// The dimensionality every returned vector must have.
    fn dimensions(&self) -> usize;
    /// Embed a batch of texts, positionally aligned with the input.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingProviderError>;
    /// Heuristic input-token estimate for cost governance (chars / 4).
    fn estimate_tokens(&self, texts: &[String]) -> u64 {
        texts
            .iter()
            .map(|text| u64::from(estimate_text_tokens(text)))
            .sum()
    }
}

/// Heuristic token count for a single text (≈ 4 chars/token, floor 1).
fn estimate_text_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(4))
        .unwrap_or(u32::MAX)
        .max(1)
}

/// A single (text → vector) recording for [`RecordingEmbeddingProvider`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingRecording {
    pub text: String,
    pub vector: Vec<f32>,
}

/// Deterministic embedding double for tests (mirrors
/// [`crate::llm_provider::RecordingProvider`]). Exact-matches input text against
/// recorded vectors; records invocations for assertions.
#[derive(Debug)]
pub struct RecordingEmbeddingProvider {
    model_id: String,
    dimensions: usize,
    recordings: Vec<EmbeddingRecording>,
    invocations: Mutex<Vec<String>>,
}

impl RecordingEmbeddingProvider {
    pub fn from_recordings(
        model_id: impl Into<String>,
        dimensions: usize,
        recordings: Vec<EmbeddingRecording>,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            dimensions,
            recordings,
            invocations: Mutex::new(Vec::new()),
        }
    }

    pub fn invocations(&self) -> Vec<String> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl EmbeddingProvider for RecordingEmbeddingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingProviderError> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            self.invocations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(text.clone());
            let recording = self
                .recordings
                .iter()
                .find(|recording| &recording.text == text)
                .ok_or(EmbeddingProviderError::MissingRecording {
                    model_id: self.model_id.clone(),
                })?;
            if recording.vector.len() != self.dimensions {
                return Err(EmbeddingProviderError::InvalidResponse {
                    message: format!(
                        "recorded vector has {} dims, expected {}",
                        recording.vector.len(),
                        self.dimensions
                    ),
                    retryable: false,
                });
            }
            out.push(recording.vector.clone());
        }
        Ok(out)
    }

    fn estimate_tokens(&self, _texts: &[String]) -> u64 {
        0
    }
}

/// Config for [`ApiEmbeddingProvider`] — an `OpenAI`-compatible `/embeddings`
/// endpoint (`OpenAI` / Voyage / Cohere-class). Mirrors
/// [`crate::llm_provider::OpenRouterProviderConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEmbeddingProviderConfig {
    pub api_key: Option<String>,
    pub allow_live_provider: bool,
    pub model_id: String,
    /// Base URL; `/embeddings` is appended.
    pub endpoint_url: String,
    pub dimensions: usize,
    pub timeout_seconds: u64,
}

/// Live embedding provider over an `OpenAI`-compatible `/embeddings` endpoint.
/// Constructed only when explicitly opted in with a key present; otherwise the
/// MCP tool degrades honestly to "not enabled".
#[derive(Debug, Clone)]
pub struct ApiEmbeddingProvider {
    model_id: String,
    api_key: String,
    endpoint_url: String,
    dimensions: usize,
    timeout_seconds: u64,
}

impl ApiEmbeddingProvider {
    pub fn from_config(config: ApiEmbeddingProviderConfig) -> Result<Self, EmbeddingProviderError> {
        if !config.allow_live_provider {
            return Err(EmbeddingProviderError::LiveProviderNotAllowed);
        }
        let Some(api_key) = config.api_key.filter(|key| !key.trim().is_empty()) else {
            return Err(EmbeddingProviderError::MissingApiKey);
        };
        if config.model_id.trim().is_empty() {
            return Err(EmbeddingProviderError::InvalidConfig {
                message: "embedding model_id must not be blank".to_owned(),
            });
        }
        if config.dimensions == 0 {
            return Err(EmbeddingProviderError::InvalidConfig {
                message: "embedding dimensions must be greater than zero".to_owned(),
            });
        }
        if config.timeout_seconds == 0 {
            return Err(EmbeddingProviderError::InvalidConfig {
                message: "embedding timeout_seconds must be greater than zero".to_owned(),
            });
        }
        Ok(Self {
            model_id: config.model_id,
            api_key,
            endpoint_url: config.endpoint_url,
            dimensions: config.dimensions,
            timeout_seconds: config.timeout_seconds,
        })
    }

    fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.endpoint_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl EmbeddingProvider for ApiEmbeddingProvider {
    fn name(&self) -> &'static str {
        "api"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingProviderError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let payload = serde_json::json!({ "model": self.model_id, "input": texts });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_seconds))
            .build()
            .map_err(|err| EmbeddingProviderError::Http {
                message: err.to_string(),
                retryable: false,
            })?;
        let response = client
            .post(self.embeddings_url())
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|err| EmbeddingProviderError::Http {
                message: err.to_string(),
                retryable: true,
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| EmbeddingProviderError::Http {
                message: err.to_string(),
                retryable: true,
            })?;
        if !status.is_success() {
            return Err(EmbeddingProviderError::Provider {
                status: status.as_u16(),
                message: body.chars().take(500).collect(),
                retryable: status.is_server_error() || status.as_u16() == 429,
            });
        }
        let parsed: EmbeddingsResponse =
            serde_json::from_str(&body).map_err(|err| EmbeddingProviderError::InvalidResponse {
                message: err.to_string(),
                retryable: true,
            })?;
        if parsed.data.len() != texts.len() {
            return Err(EmbeddingProviderError::InvalidResponse {
                message: format!(
                    "response returned {} embeddings for {} inputs",
                    parsed.data.len(),
                    texts.len()
                ),
                retryable: false,
            });
        }
        // Restore positional alignment: the API may return rows out of order,
        // but each carries its input `index`.
        let mut ordered: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for row in parsed.data {
            let Some(slot) = ordered.get_mut(row.index) else {
                return Err(EmbeddingProviderError::InvalidResponse {
                    message: format!("embedding index {} out of range", row.index),
                    retryable: false,
                });
            };
            if row.embedding.len() != self.dimensions {
                return Err(EmbeddingProviderError::InvalidResponse {
                    message: format!(
                        "embedding has {} dims, expected {}",
                        row.embedding.len(),
                        self.dimensions
                    ),
                    retryable: false,
                });
            }
            *slot = Some(row.embedding);
        }
        ordered
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                slot.ok_or(EmbeddingProviderError::InvalidResponse {
                    message: format!("missing embedding for input index {i}"),
                    retryable: false,
                })
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingDatum {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(text: &str, vector: Vec<f32>) -> EmbeddingRecording {
        EmbeddingRecording {
            text: text.to_owned(),
            vector,
        }
    }

    #[tokio::test]
    async fn recording_provider_returns_recorded_vectors_in_order() {
        let provider = RecordingEmbeddingProvider::from_recordings(
            "test-model",
            2,
            vec![rec("alpha", vec![1.0, 0.0]), rec("beta", vec![0.0, 1.0])],
        );
        let out = provider
            .embed(&["beta".to_owned(), "alpha".to_owned()])
            .await
            .expect("embed");
        assert_eq!(out, vec![vec![0.0, 1.0], vec![1.0, 0.0]]);
        assert_eq!(provider.invocations(), vec!["beta", "alpha"]);
        assert_eq!(provider.dimensions(), 2);
        assert_eq!(provider.model_id(), "test-model");
    }

    #[tokio::test]
    async fn recording_provider_errors_on_missing_text() {
        let provider =
            RecordingEmbeddingProvider::from_recordings("m", 1, vec![rec("known", vec![1.0])]);
        let err = provider.embed(&["unknown".to_owned()]).await.unwrap_err();
        assert!(matches!(
            err,
            EmbeddingProviderError::MissingRecording { .. }
        ));
        assert!(!err.retryable());
    }

    #[tokio::test]
    async fn recording_provider_rejects_wrong_dimension() {
        let provider =
            RecordingEmbeddingProvider::from_recordings("m", 3, vec![rec("x", vec![1.0, 2.0])]);
        let err = provider.embed(&["x".to_owned()]).await.unwrap_err();
        assert!(matches!(
            err,
            EmbeddingProviderError::InvalidResponse { .. }
        ));
    }

    #[test]
    fn api_provider_refuses_without_opt_in() {
        let err = ApiEmbeddingProvider::from_config(ApiEmbeddingProviderConfig {
            api_key: Some("k".to_owned()),
            allow_live_provider: false,
            model_id: "m".to_owned(),
            endpoint_url: "https://example".to_owned(),
            dimensions: 8,
            timeout_seconds: 30,
        })
        .unwrap_err();
        assert_eq!(err, EmbeddingProviderError::LiveProviderNotAllowed);
    }

    #[test]
    fn api_provider_refuses_without_key() {
        let err = ApiEmbeddingProvider::from_config(ApiEmbeddingProviderConfig {
            api_key: None,
            allow_live_provider: true,
            model_id: "m".to_owned(),
            endpoint_url: "https://example".to_owned(),
            dimensions: 8,
            timeout_seconds: 30,
        })
        .unwrap_err();
        assert_eq!(err, EmbeddingProviderError::MissingApiKey);
    }

    #[test]
    fn api_provider_validates_dims_and_timeout() {
        let base = ApiEmbeddingProviderConfig {
            api_key: Some("k".to_owned()),
            allow_live_provider: true,
            model_id: "m".to_owned(),
            endpoint_url: "https://example".to_owned(),
            dimensions: 0,
            timeout_seconds: 30,
        };
        assert!(matches!(
            ApiEmbeddingProvider::from_config(base.clone()).unwrap_err(),
            EmbeddingProviderError::InvalidConfig { .. }
        ));
        assert!(
            ApiEmbeddingProvider::from_config(ApiEmbeddingProviderConfig {
                dimensions: 8,
                ..base
            })
            .is_ok()
        );
    }
}
