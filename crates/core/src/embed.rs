//! Embedding providers. `FastEmbedder` is the real one (fastembed-rs over ONNX Runtime);
//! `HashEmbedder` is a deterministic bag-of-words stand-in used by tests and offline smoke runs.

use anyhow::Result;
use std::path::Path;

pub trait Embedder: Send + Sync {
    fn model(&self) -> &str;
    fn dim(&self) -> usize;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub name: &'static str,
    pub dim: usize,
    pub model: fastembed::EmbeddingModel,
}

pub const DEFAULT_MODEL: &str = "Xenova/all-MiniLM-L6-v2";

/// Maps the TS `localModelName` values to fastembed models. Unknown names fall back to MiniLM.
pub fn model_info(name: &str) -> ModelInfo {
    use fastembed::EmbeddingModel as M;
    let (name, dim, model) = match name {
        "BAAI/bge-small-en-v1.5" => ("BAAI/bge-small-en-v1.5", 384, M::BGESmallENV15),
        "BAAI/bge-base-en-v1.5" => ("BAAI/bge-base-en-v1.5", 768, M::BGEBaseENV15),
        "Xenova/all-mpnet-base-v2" => ("Xenova/all-mpnet-base-v2", 768, M::AllMpnetBaseV2),
        "sentence-transformers/all-MiniLM-L6-v2" => (
            "sentence-transformers/all-MiniLM-L6-v2",
            384,
            M::AllMiniLML6V2,
        ),
        _ => (DEFAULT_MODEL, 384, M::AllMiniLML6V2),
    };
    ModelInfo { name, dim, model }
}

/// ORT keeps its peak allocation in an arena that never shrinks, and each batch pads to its
/// longest text. Batches of 64 × 512 tokens held ~4.7 GB after a 34k-chunk index on Windows;
/// 16 × 256 caps the padded attention buffers at 1/16 of that shape.
const ORT_BATCH: usize = 16;
const MAX_TOKENS: usize = 256;

pub struct FastEmbedder {
    info: ModelInfo,
    // fastembed's embed takes &mut self; ORT parallelizes inside one call, so a mutex costs nothing.
    model: std::sync::Mutex<fastembed::TextEmbedding>,
}

impl FastEmbedder {
    /// Loads (downloading on first use into `cache_dir`) the ONNX model.
    pub fn new(name: &str, cache_dir: &Path) -> Result<FastEmbedder> {
        let info = model_info(name);
        let opts = fastembed::TextInitOptions::new(info.model.clone())
            .with_cache_dir(cache_dir.to_path_buf())
            .with_show_download_progress(false)
            // MiniLM/BGE are trained on ≤256 tokens and our chunks are ~128; 512 only inflates
            // the padded attention buffers.
            .with_max_length(MAX_TOKENS);
        let model = fastembed::TextEmbedding::try_new(opts)
            .map_err(|e| anyhow::anyhow!("loading {}: {e}", info.name))?;
        Ok(FastEmbedder {
            info,
            model: std::sync::Mutex::new(model),
        })
    }
}

impl Embedder for FastEmbedder {
    fn model(&self) -> &str {
        self.info.name
    }
    fn dim(&self) -> usize {
        self.info.dim
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut m = self.model.lock().unwrap_or_else(|e| e.into_inner());
        m.embed(texts, Some(ORT_BATCH))
            .map_err(|e| anyhow::anyhow!("embedding failed: {e}"))
    }
}

pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Embedder for HashEmbedder {
    fn model(&self) -> &str {
        "hash"
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; self.dim];
                v[0] = 1e-3; // keeps empty text a valid (non-zero) vector
                for w in t
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                {
                    v[(blake3::hash(w.as_bytes()).as_bytes()[0] as usize * 131 + w.len())
                        % self.dim] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cos(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn model_info_maps_ts_names() {
        assert_eq!(model_info("Xenova/all-MiniLM-L6-v2").dim, 384);
        assert_eq!(
            model_info("BAAI/bge-small-en-v1.5").model,
            fastembed::EmbeddingModel::BGESmallENV15
        );
        assert_eq!(model_info("BAAI/bge-base-en-v1.5").dim, 768);
        assert_eq!(
            model_info("sentence-transformers/all-MiniLM-L6-v2").model,
            fastembed::EmbeddingModel::AllMiniLML6V2
        );
        assert_eq!(model_info("nope/unknown").name, DEFAULT_MODEL);
    }

    #[test]
    fn hash_embedder_is_deterministic_normalized_and_sized() {
        let e = HashEmbedder::new(64);
        let v = e
            .embed(&[
                "forge anvil hammer".into(),
                "forge anvil hammer".into(),
                String::new(),
            ])
            .unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], v[1]);
        assert!(v.iter().all(|x| x.len() == 64));
        assert!((cos(&v[0], &v[0]) - 1.0).abs() < 1e-5);
        assert!(
            (cos(&v[2], &v[2]) - 1.0).abs() < 1e-5,
            "empty text still gets a unit vector"
        );
    }

    #[test]
    fn hash_embedder_places_shared_words_closer() {
        let e = HashEmbedder::new(256);
        let v = e
            .embed(&[
                "rust borrow checker".into(),
                "the rust checker".into(),
                "tomato garden soil".into(),
            ])
            .unwrap();
        assert!(cos(&v[0], &v[1]) > cos(&v[0], &v[2]));
    }

    #[test]
    #[ignore = "downloads ~90 MB model; run in verification with --ignored"]
    fn fastembed_minilm_produces_384d_unit_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let e = FastEmbedder::new(DEFAULT_MODEL, dir.path()).unwrap();
        let v = e
            .embed(&["hello world".into(), "hola mundo".into()])
            .unwrap();
        assert_eq!((e.dim(), v.len(), v[0].len()), (384, 2, 384));
        assert!((cos(&v[0], &v[0]) - 1.0).abs() < 1e-3);
    }
}
