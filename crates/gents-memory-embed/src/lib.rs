//! Bundled in-process multilingual embeddings for agent memory.
//!
//! The weights are `minishlab/blink-multilingual-64M-int8` (MIT), compiled into
//! the binary, so memory works with no network and no external service. The
//! model emits 256-dimensional normalized static embeddings.

use std::sync::{Arc, Mutex};

use model2vec_rs::model::StaticModel;

pub const MODEL_ID: &str = "minishlab/blink-multilingual-64M-int8";
pub const DIMENSIONS: usize = 256;

const CONFIG: &[u8] = include_bytes!("../assets/blink-multilingual-64M-int8/config.json");
const TOKENIZER: &[u8] = include_bytes!("../assets/blink-multilingual-64M-int8/tokenizer.json");
const WEIGHTS: &[u8] = include_bytes!("../assets/blink-multilingual-64M-int8/model.safetensors");

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("the bundled embedding model could not be loaded")]
    Load(String),
    #[error("the embedding worker did not finish")]
    Join(String),
}

#[derive(Clone)]
pub struct StaticEmbedder {
    model: Arc<Mutex<StaticModel>>,
}

impl StaticEmbedder {
    pub async fn load() -> Result<Self, EmbedError> {
        tokio::task::spawn_blocking(|| {
            StaticModel::from_bytes(TOKENIZER, WEIGHTS, CONFIG, None)
                .map(|model| Self {
                    model: Arc::new(Mutex::new(model)),
                })
                .map_err(|error| EmbedError::Load(format!("{error:#}")))
        })
        .await
        .map_err(|error| EmbedError::Join(error.to_string()))?
    }

    pub fn model_id(&self) -> &'static str {
        MODEL_ID
    }

    pub fn dimensions(&self) -> usize {
        DIMENSIONS
    }

    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        let model = Arc::clone(&self.model);
        tokio::task::spawn_blocking(move || {
            let model = model
                .lock()
                .map_err(|error| EmbedError::Load(error.to_string()))?;
            Ok(model.encode(&texts))
        })
        .await
        .map_err(|error| EmbedError::Join(error.to_string()))?
    }

    pub async fn embed_one(&self, text: impl Into<String>) -> Result<Vec<f32>, EmbedError> {
        let model = Arc::clone(&self.model);
        let text = text.into();
        tokio::task::spawn_blocking(move || {
            let model = model
                .lock()
                .map_err(|error| EmbedError::Load(error.to_string()))?;
            Ok(model.encode_single(&text))
        })
        .await
        .map_err(|error| EmbedError::Join(error.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(vector: &[f32]) -> f32 {
        vector.iter().map(|value| value * value).sum::<f32>().sqrt()
    }

    #[tokio::test]
    async fn bundled_model_embeds_normalized_multilingual_vectors() {
        let embedder = StaticEmbedder::load().await.expect("model loads");
        assert_eq!(embedder.model_id(), MODEL_ID);
        assert_eq!(embedder.dimensions(), DIMENSIONS);

        let first = embedder
            .embed_one("The release notes go to #eng-releases until the deploy is green.")
            .await
            .expect("one embedding");
        assert_eq!(first.len(), DIMENSIONS);
        assert!(first.iter().all(|value| value.is_finite()));
        assert!((norm(&first) - 1.0).abs() < 0.02, "norm {}", norm(&first));

        let batch = embedder
            .embed(vec![
                "Hallo wereld".to_owned(),
                "Bonjour le monde".to_owned(),
            ])
            .await
            .expect("batch");
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(|vector| vector.len() == DIMENSIONS));
        assert!(batch[0] != batch[1]);
    }
}
