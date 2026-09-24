use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileTypes {
    pub markdown: bool,
    pub pdf: bool,
    pub docx: bool,
    pub html: bool,
}

impl Default for FileTypes {
    fn default() -> Self {
        Self { markdown: true, pdf: true, docx: true, html: false }
    }
}

/// Same keys and location as the TS `config.json`. Unknown keys (e.g. the dropped
/// OpenAI settings) are ignored on load and not written back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub watch_dirs: Vec<String>,
    pub embedding_provider: String,
    pub local_model_name: String,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub exclude_patterns: Vec<String>,
    pub auto_index_on_change: bool,
    pub indexing_debounce_ms: u64,
    pub file_types: FileTypes,
    pub hybrid_search: bool,
    pub search_results_limit: usize,
    pub importance_weight: f64,
    pub dir_exclude_patterns: BTreeMap<String, Vec<String>>,
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    pub data_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            watch_dirs: vec![],
            embedding_provider: "local".into(),
            local_model_name: "Xenova/all-MiniLM-L6-v2".into(),
            chunk_size: 512,
            chunk_overlap: 64,
            exclude_patterns: [".git", "node_modules", ".obsidian", ".claude"].map(String::from).to_vec(),
            auto_index_on_change: true,
            indexing_debounce_ms: 5_000,
            file_types: FileTypes::default(),
            hybrid_search: true,
            search_results_limit: 35,
            importance_weight: 0.05,
            dir_exclude_patterns: BTreeMap::new(),
            mcp_enabled: true,
            mcp_port: 8867,
            data_dir: String::new(),
        }
    }
}

pub fn default_config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("anamnesis").join("config.json")
}

impl Config {
    /// Missing or unparsable file → defaults (logged), never an error: the app must boot.
    pub fn load(path: &Path) -> Config {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str::<Config>(&raw).unwrap_or_else(|e| {
                tracing::warn!("could not parse config at {}: {e}", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        if cfg.data_dir.is_empty() {
            cfg.data_dir = path.parent().unwrap_or(Path::new(".")).join("data").to_string_lossy().into();
        }
        cfg.clamp();
        cfg
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Shallow-merge a partial JSON object (what the UI's saveConfig sends) over this config.
    pub fn merged(&self, partial: &serde_json::Value) -> anyhow::Result<Config> {
        let mut base = serde_json::to_value(self)?;
        let (Some(b), Some(p)) = (base.as_object_mut(), partial.as_object()) else {
            anyhow::bail!("config update must be a JSON object");
        };
        for (k, v) in p {
            b.insert(k.clone(), v.clone());
        }
        let mut cfg: Config = serde_json::from_value(base)?;
        cfg.clamp();
        Ok(cfg)
    }

    /// Same bounds as the TS zod schema, but clamped instead of discarding the whole file.
    fn clamp(&mut self) {
        self.chunk_size = self.chunk_size.clamp(64, 4096);
        self.chunk_overlap = self.chunk_overlap.min(512).min(self.chunk_size / 2);
        self.indexing_debounce_ms = self.indexing_debounce_ms.clamp(500, 300_000);
        self.search_results_limit = self.search_results_limit.clamp(1, 100);
        self.importance_weight = self.importance_weight.clamp(0.0, 1.0);
        self.mcp_port = self.mcp_port.max(1024);
        self.embedding_provider = "local".into();
    }

    pub fn models_dir(&self) -> PathBuf {
        Path::new(&self.data_dir).join("models")
    }

    pub fn db_path(&self) -> PathBuf {
        Path::new(&self.data_dir).join("anamnesis.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_file_yields_defaults_with_data_dir_next_to_config() {
        let d = tmp();
        let p = d.path().join("config.json");
        let c = Config::load(&p);
        assert_eq!(c.chunk_size, 512);
        assert_eq!(c.mcp_port, 8867);
        assert_eq!(c.exclude_patterns, vec![".git", "node_modules", ".obsidian", ".claude"]);
        assert_eq!(PathBuf::from(&c.data_dir), d.path().join("data"));
    }

    #[test]
    fn corrupt_json_falls_back_to_defaults() {
        let d = tmp();
        let p = d.path().join("config.json");
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(Config::load(&p).search_results_limit, 35);
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_keys_and_ignores_unknown_keys() {
        let d = tmp();
        let p = d.path().join("config.json");
        std::fs::write(&p, r#"{"watchDirs":["/v"],"openaiApiKey":"sk-x","embeddingProvider":"openai","fileTypes":{"html":true}}"#).unwrap();
        let c = Config::load(&p);
        assert_eq!(c.watch_dirs, vec!["/v"]);
        assert_eq!(c.embedding_provider, "local", "OpenAI provider was dropped");
        assert!(c.file_types.html && c.file_types.markdown, "nested defaults survive");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let d = tmp();
        let p = d.path().join("config.json");
        std::fs::write(&p, r#"{"chunkSize":10,"chunkOverlap":9999,"indexingDebounceMs":1,"searchResultsLimit":500,"importanceWeight":3.0,"mcpPort":80}"#).unwrap();
        let c = Config::load(&p);
        assert_eq!(c.chunk_size, 64);
        assert_eq!(c.chunk_overlap, 32, "overlap never exceeds half the chunk");
        assert_eq!(c.indexing_debounce_ms, 500);
        assert_eq!(c.search_results_limit, 100);
        assert_eq!(c.importance_weight, 1.0);
        assert_eq!(c.mcp_port, 1024);
    }

    #[test]
    fn save_then_load_roundtrips_and_uses_camel_case_keys() {
        let d = tmp();
        let p = d.path().join("nested").join("config.json");
        let mut c = Config::load(&p);
        c.watch_dirs = vec!["/notes".into()];
        c.save(&p).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"watchDirs\"") && raw.contains("\"mcpPort\""));
        assert_eq!(Config::load(&p), c);
    }

    #[test]
    fn merged_applies_partial_update_and_rejects_non_objects() {
        let c = Config::default();
        let m = c.merged(&json!({"hybridSearch": false, "dirExcludePatterns": {"/v": ["*.pdf"]}})).unwrap();
        assert!(!m.hybrid_search);
        assert_eq!(m.dir_exclude_patterns["/v"], vec!["*.pdf"]);
        assert_eq!(m.chunk_size, c.chunk_size);
        assert!(c.merged(&json!([1, 2])).is_err());
        assert!(c.merged(&json!({"chunkSize": "big"})).is_err(), "type errors surface to the UI");
    }
}
