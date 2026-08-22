use fastembed::{
    InitOptionsUserDefined, Pooling, QuantizationMode, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use std::path::PathBuf;

const MODEL_REPO: &str = "Xenova/bge-small-zh-v1.5";
const ENDPOINTS: [&str; 2] = [
    "https://hf-mirror.com",
    "https://huggingface.co",
];

const MODEL_FILES: [&str; 5] = [
    "onnx/model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

pub struct EmbeddingService {
    model: TextEmbedding,
}

impl EmbeddingService {
    pub fn new(models_dir: PathBuf, resource_dir: Option<PathBuf>) -> Result<Self, String> {
        let model_dir = models_dir.join("bge-small-zh-v1.5");

        if !MODEL_FILES.iter().all(|f| model_dir.join(f).exists()) {
            if let Some(ref res_dir) = resource_dir {
                let res_model_dir = res_dir.join("model");
                tracing::info!(
                    res_dir = %res_model_dir.display(),
                    "checking bundled model resources"
                );
                if MODEL_FILES.iter().all(|f| res_model_dir.join(f).exists()) {
                    std::fs::create_dir_all(model_dir.join("onnx"))
                        .map_err(|e| format!("create model dir: {e}"))?;
                    for f in &MODEL_FILES {
                        let src = res_model_dir.join(f);
                        let dst = model_dir.join(f);
                        if !dst.exists() {
                            tracing::info!(file = f, "copying from resources");
                            std::fs::copy(&src, &dst)
                                .map_err(|e| format!("copy {f}: {e}"))?;
                        }
                    }
                    tracing::info!("model files copied from bundled resources");
                }
            }

            if !MODEL_FILES.iter().all(|f| model_dir.join(f).exists()) {
                tracing::info!("model files missing, downloading...");
                Self::download_model(&model_dir)?;
            }
        }

        Self::load_from_dir(&model_dir)
    }

    fn load_from_dir(model_dir: &PathBuf) -> Result<Self, String> {
        let onnx_file = std::fs::read(model_dir.join("onnx/model.onnx"))
            .map_err(|e| format!("read model.onnx: {e}"))?;

        let tokenizer_files = TokenizerFiles {
            tokenizer_file: std::fs::read(model_dir.join("tokenizer.json"))
                .map_err(|e| format!("read tokenizer.json: {e}"))?,
            config_file: std::fs::read(model_dir.join("config.json"))
                .map_err(|e| format!("read config.json: {e}"))?,
            special_tokens_map_file: std::fs::read(model_dir.join("special_tokens_map.json"))
                .map_err(|e| format!("read special_tokens_map.json: {e}"))?,
            tokenizer_config_file: std::fs::read(model_dir.join("tokenizer_config.json"))
                .map_err(|e| format!("read tokenizer_config.json: {e}"))?,
        };

        tracing::info!(onnx_size = onnx_file.len(), "loaded model files");

        let model = UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files)
            .with_pooling(Pooling::Cls)
            .with_quantization(QuantizationMode::None);

        let model = TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new())
            .map_err(|e| format!("init embedding model: {e}"))?;

        tracing::info!("embedding model loaded successfully");
        Ok(Self { model })
    }

    fn download_model(model_dir: &PathBuf) -> Result<(), String> {
        std::fs::create_dir_all(model_dir.join("onnx"))
            .map_err(|e| format!("create model dir: {e}"))?;

        for file in &MODEL_FILES {
            let dest = model_dir.join(file);
            if dest.exists() {
                continue;
            }

            let mut last_err = String::new();
            for endpoint in &ENDPOINTS {
                let url = format!("{}/{MODEL_REPO}/resolve/main/{file}", endpoint);
                tracing::info!(url = %url, "downloading model file");

                match Self::download_file(&url, &dest) {
                    Ok(()) => {
                        tracing::info!(file, "downloaded successfully");
                        last_err.clear();
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(url = %url, err = %e, "download failed, trying next endpoint");
                        last_err = e;
                    }
                }
            }

            if !last_err.is_empty() {
                return Err(format!("failed to download {file}: {last_err}"));
            }
        }

        tracing::info!("all model files downloaded");
        Ok(())
    }

    fn download_file(url: &str, dest: &PathBuf) -> Result<(), String> {
        let dest_str = dest.to_string_lossy();
        let output = std::process::Command::new("curl.exe")
            .args([
                "-L", "-o", &dest_str,
                "--connect-timeout", "30",
                "--max-time", "300",
                "--retry", "2",
                "-s", "-S",
                "-w", "%{http_code}",
                url,
            ])
            .output()
            .map_err(|e| format!("failed to run curl: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("curl failed: {}", stderr.trim()));
        }

        if !dest.exists() {
            return Err("file not created after download".to_string());
        }

        let http_code = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !http_code.starts_with('2') {
            return Err(format!("HTTP {}", http_code));
        }

        Ok(())
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
