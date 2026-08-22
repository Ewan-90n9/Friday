use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;

pub struct EmbeddingService {
    model: TextEmbedding,
}

impl EmbeddingService {
    pub fn new(models_dir: PathBuf) -> Result<Self, String> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallZHV15).with_cache_dir(models_dir),
        )
        .map_err(|e| format!("failed to load embedding model: {e}"))?;
        Ok(Self { model })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let embeddings = self
            .model
            .embed(vec![text.to_string()], None)
            .map_err(|e| format!("embedding inference failed: {e}"))?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| "embedding returned no results".to_string())
    }
}
