use fastembed::{
    InitOptionsUserDefined, Pooling, QuantizationMode, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use std::io::Read;
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

fn detect_proxy() -> Option<String> {
    for var in &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(proxy) = std::env::var(var) {
            if !proxy.is_empty() {
                return Some(proxy);
            }
        }
    }

    #[cfg(windows)]
    {
        use winreg::enums::*;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(settings) = hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings") {
            let proxy_enable: u32 = settings.get_value("ProxyEnable").unwrap_or(0);
            if proxy_enable == 1 {
                let proxy_server: String = settings.get_value("ProxyServer").unwrap_or_default();
                if !proxy_server.is_empty() {
                    if proxy_server.starts_with("http://") || proxy_server.starts_with("https://") {
                        return Some(proxy_server);
                    } else {
                        return Some(format!("http://{}", proxy_server));
                    }
                }
            }
        }
    }

    None
}

pub struct EmbeddingService {
    model: TextEmbedding,
}

impl EmbeddingService {
    pub fn new(models_dir: PathBuf) -> Result<Self, String> {
        let model_dir = models_dir.join("bge-small-zh-v1.5");

        let need_download = !MODEL_FILES
            .iter()
            .all(|f| model_dir.join(f).exists());

        if need_download {
            tracing::info!("model files missing, downloading...");
            Self::download_model(&model_dir)?;
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
        let mut builder = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(120));

        if let Some(proxy) = detect_proxy() {
            tracing::info!(proxy = %proxy, "using proxy for model download");
            builder = builder.proxy(
                ureq::Proxy::new(&proxy).map_err(|e| format!("invalid proxy: {e}"))?
            );
        }

        let agent = builder.build();

        let response = agent
            .get(url)
            .call()
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let mut reader = response.into_reader();
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|e| format!("read response body: {e}"))?;

        std::fs::write(dest, &buf)
            .map_err(|e| format!("write file: {e}"))?;

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
