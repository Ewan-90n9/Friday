use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, QuantizationMode, TextEmbedding,
    TokenizerFiles, UserDefinedEmbeddingModel,
};
use std::path::PathBuf;

pub struct EmbeddingService {
    model: TextEmbedding,
}

impl EmbeddingService {
    pub fn new(models_dir: PathBuf) -> Result<Self, String> {
        let repo_dir = models_dir.join("models--Xenova--bge-small-zh-v1.5");
        tracing::info!(
            models_dir = %models_dir.display(),
            repo_dir = %repo_dir.display(),
            repo_exists = repo_dir.exists(),
            "checking for cached embedding model"
        );

        if repo_dir.exists() {
            tracing::info!("found cached model, loading from local files");
            Self::load_from_cache(&repo_dir)
        } else {
            tracing::info!("model not cached, downloading from HuggingFace");
            Self::download_and_load(models_dir)
        }
    }

    fn load_from_cache(repo_dir: &PathBuf) -> Result<Self, String> {
        let refs_dir = repo_dir.join("refs");
        let snapshots_dir = repo_dir.join("snapshots");

        let commit_hash = std::fs::read_to_string(refs_dir.join("main"))
            .map_err(|e| format!("failed to read refs/main: {e}"))?
            .trim()
            .to_string();

        let snapshot_dir = snapshots_dir.join(&commit_hash);
        tracing::info!(snapshot_dir = %snapshot_dir.display(), "loading model from snapshot");

        let onnx_path = snapshot_dir.join("onnx").join("model.onnx");
        let tokenizer_path = snapshot_dir.join("tokenizer.json");
        let config_path = snapshot_dir.join("config.json");
        let special_tokens_path = snapshot_dir.join("special_tokens_map.json");
        let tokenizer_config_path = snapshot_dir.join("tokenizer_config.json");

        tracing::info!(onnx_path = %onnx_path.display(), exists = onnx_path.exists(), "checking onnx file");

        let onnx_file = std::fs::read(&onnx_path)
            .map_err(|e| format!("failed to read model.onnx at {}: {e}", onnx_path.display()))?;

        let tokenizer_files = TokenizerFiles {
            tokenizer_file: std::fs::read(&tokenizer_path)
                .map_err(|e| format!("failed to read tokenizer.json: {e}"))?,
            config_file: std::fs::read(&config_path)
                .map_err(|e| format!("failed to read config.json: {e}"))?,
            special_tokens_map_file: std::fs::read(&special_tokens_path)
                .map_err(|e| format!("failed to read special_tokens_map.json: {e}"))?,
            tokenizer_config_file: std::fs::read(&tokenizer_config_path)
                .map_err(|e| format!("failed to read tokenizer_config.json: {e}"))?,
        };

        tracing::info!(onnx_size = onnx_file.len(), "loaded onnx model bytes");

        let model = UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files)
            .with_pooling(Pooling::Cls)
            .with_quantization(QuantizationMode::None);

        let model = TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new())
            .map_err(|e| format!("failed to init embedding model from cache: {e}"))?;

        tracing::info!("embedding model loaded successfully from local cache");
        Ok(Self { model })
    }

    fn download_and_load(models_dir: PathBuf) -> Result<Self, String> {
        if std::env::var("HF_ENDPOINT").is_err() {
            std::env::set_var("HF_ENDPOINT", "https://hf-mirror.com");
            tracing::info!("set HF_ENDPOINT to https://hf-mirror.com for model download");
        }

        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallZHV15)
                .with_cache_dir(models_dir)
                .with_show_download_progress(true),
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
