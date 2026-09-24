//! Hybrid search: sqlite-vec KNN + FTS5 BM25, fused with Reciprocal Rank Fusion (k = 60).

use crate::config::Config;
use crate::embed::Embedder;
use crate::store::{Hit, Store};
use anyhow::Result;
use serde::Serialize;
use std::sync::{Arc, RwLock};

/// Everything a search needs, shared by the MCP server, the UI commands and the indexer's owner.
#[derive(Clone)]
pub struct Searcher {
    pub store: Arc<Store>,
    pub embedder: Arc<dyn Embedder>,
    pub cfg: Arc<RwLock<Config>>,
}

impl Searcher {
    /// Embeds the query and runs a hybrid search with the configured options. Blocking (ORT).
    pub fn search(&self, query: &str, limit: Option<usize>) -> Result<Vec<SearchHit>> {
        let (opts, default_limit) = {
            let c = self.cfg.read().unwrap_or_else(|e| e.into_inner());
            (
                SearchOpts {
                    hybrid: c.hybrid_search,
                    importance_weight: c.importance_weight,
                },
                c.search_results_limit,
            )
        };
        let limit = limit.unwrap_or(default_limit).clamp(1, 100);
        let qv = self.embedder.embed(&[query.to_string()])?.remove(0);
        search(&self.store, &qv, query, limit, opts)
    }
}

pub const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, Copy)]
pub struct SearchOpts {
    pub hybrid: bool,
    pub importance_weight: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchHit {
    #[serde(flatten)]
    pub hit: Hit,
    pub match_sources: Vec<&'static str>,
    /// 0..1 similarity from the cosine distance; `None` for BM25-only matches.
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fused {
    pub id: i64,
    pub sources: Vec<&'static str>,
    pub distance: Option<f64>,
}

/// Turns free text into a safe FTS5 MATCH expression: every word quoted, OR-ed.
/// Returns `None` when the query has no searchable words.
pub fn fts_query(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

/// `semantic` is (id, distance, importance) in KNN order; `bm25` is ids in BM25 order.
pub fn fuse(
    semantic: &[(i64, f64, i64)],
    bm25: &[i64],
    limit: usize,
    opts: SearchOpts,
) -> Vec<Fused> {
    let mut sem = semantic.to_vec();
    if opts.importance_weight > 0.0 {
        let boosted =
            |&(_, d, imp): &(i64, f64, i64)| d - opts.importance_weight * (1.0 + imp as f64).ln();
        sem.sort_by(|a, b| boosted(a).total_cmp(&boosted(b)));
    }
    if !opts.hybrid {
        return sem
            .iter()
            .take(limit)
            .map(|&(id, d, _)| Fused {
                id,
                sources: vec!["semantic"],
                distance: Some(d),
            })
            .collect();
    }
    let mut fused: Vec<(Fused, f64)> = Vec::new();
    let mut add = |id: i64, rank: usize, source: &'static str, distance: Option<f64>| {
        let score = 1.0 / (RRF_K + rank as f64 + 1.0);
        match fused.iter_mut().find(|(f, _)| f.id == id) {
            Some((f, s)) => {
                f.sources.push(source);
                *s += score;
            }
            None => fused.push((
                Fused {
                    id,
                    sources: vec![source],
                    distance,
                },
                score,
            )),
        }
    };
    for (rank, &(id, d, _)) in sem.iter().enumerate() {
        add(id, rank, "semantic", Some(d));
    }
    for (rank, &id) in bm25.iter().enumerate() {
        add(id, rank, "bm25", None);
    }
    // ponytail: linear find in a few hundred candidates (limit·3 per side), fine; HashMap if limits grow.
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
    fused.into_iter().take(limit).map(|(f, _)| f).collect()
}

pub fn search(
    store: &Store,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    opts: SearchOpts,
) -> Result<Vec<SearchHit>> {
    let fetch = if opts.hybrid { limit * 3 } else { limit };
    let knn = store.knn(query_vec, fetch)?;
    let ids: Vec<i64> = knn.iter().map(|(id, _)| *id).collect();
    let bm25 = match (opts.hybrid, fts_query(query)) {
        (true, Some(q)) => store.fts(&q, fetch)?,
        _ => vec![],
    };
    let mut hits = store.hits(&[ids, bm25.clone()].concat())?;
    let semantic: Vec<(i64, f64, i64)> = knn
        .iter()
        .filter_map(|&(id, d)| hits.get(&id).map(|h| (id, d, h.importance_score)))
        .collect();
    Ok(fuse(&semantic, &bm25, limit, opts)
        .into_iter()
        .filter_map(|f| {
            hits.remove(&f.id).map(|hit| SearchHit {
                hit,
                match_sources: f.sources,
                // Same mapping as the TS version: distance in [0, 2] → score in [0, 1].
                score: f.distance.map(|d| (1.0 - d / 2.0).max(0.0)),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ChunkRow, FileRecord};

    const HYBRID: SearchOpts = SearchOpts {
        hybrid: true,
        importance_weight: 0.0,
    };
    const SEMANTIC: SearchOpts = SearchOpts {
        hybrid: false,
        importance_weight: 0.0,
    };

    #[test]
    fn fts_query_quotes_words_and_drops_syntax() {
        assert_eq!(
            fts_query("rust ownership").as_deref(),
            Some("\"rust\" OR \"ownership\"")
        );
        assert_eq!(
            fts_query("  \"AND\" (NEAR* -x:y) ").as_deref(),
            Some("\"AND\" OR \"NEAR\" OR \"x\" OR \"y\"")
        );
        assert_eq!(
            fts_query("reflexión área").as_deref(),
            Some("\"reflexión\" OR \"área\"")
        );
        assert_eq!(fts_query("  ?!  "), None);
        assert_eq!(fts_query(""), None);
    }

    #[test]
    fn fuse_semantic_only_keeps_knn_order_and_limit() {
        let sem = [(1, 0.1, 0), (2, 0.2, 0), (3, 0.3, 0)];
        let out = fuse(&sem, &[3], 2, SEMANTIC);
        assert_eq!(out.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(
            out.iter().all(|f| f.sources == vec!["semantic"]),
            "bm25 ignored when hybrid is off"
        );
        assert_eq!(out[0].distance, Some(0.1));
    }

    #[test]
    fn fuse_rrf_rewards_items_found_by_both_rankers() {
        let sem = [(1, 0.1, 0), (2, 0.2, 0), (3, 0.3, 0)];
        let bm25 = [3, 4];
        let out = fuse(&sem, &bm25, 10, HYBRID);
        assert_eq!(out[0].id, 3, "rank 3 + rank 1 beats rank 1 alone");
        assert_eq!(out[0].sources, vec!["semantic", "bm25"]);
        let four = out.iter().find(|f| f.id == 4).unwrap();
        assert_eq!(four.sources, vec!["bm25"]);
        assert_eq!(four.distance, None);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn fuse_rrf_scores_match_formula() {
        // id 1: sem rank 1 → 1/61 ; id 2: bm25 rank 1 → 1/61 ; tie broken by id for determinism.
        let out = fuse(&[(1, 0.5, 0)], &[2], 10, HYBRID);
        assert_eq!(out.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn fuse_importance_boost_reorders_semantic_hits() {
        let sem = [(1, 0.30, 0), (2, 0.32, 5)];
        let boosted = SearchOpts {
            hybrid: false,
            importance_weight: 0.05,
        };
        assert_eq!(fuse(&sem, &[], 10, SEMANTIC)[0].id, 1);
        assert_eq!(
            fuse(&sem, &[], 10, boosted)[0].id,
            2,
            "0.32 - 0.05·ln 6 < 0.30"
        );
    }

    #[test]
    fn fuse_empty_inputs() {
        assert!(fuse(&[], &[], 10, HYBRID).is_empty());
        assert!(fuse(&[(1, 0.1, 0)], &[], 0, HYBRID).is_empty());
    }

    fn seeded() -> Store {
        let s = Store::open_in_memory("m", 2).unwrap();
        let rows = [
            ("rust borrow checker", [1.0, 0.0]),
            ("gardening tomatoes", [0.0, 1.0]),
            ("rust in iron pipes", [0.1, 1.0]),
        ];
        let chunks = rows
            .iter()
            .enumerate()
            .map(|(i, (t, v))| ChunkRow {
                chunk_index: i,
                heading: String::new(),
                context_path: String::new(),
                text: t.to_string(),
                embed_hash: format!("h{i}"),
                vector: v.to_vec(),
            })
            .collect();
        s.replace_file(&FileRecord {
            path: "/v/a.md".into(),
            mtime_ns: 1,
            content_hash: "c".into(),
            tags: String::new(),
            chunks,
        })
        .unwrap();
        s
    }

    #[test]
    fn search_hybrid_combines_vector_and_keyword_hits() {
        let s = seeded();
        let out = search(&s, &[1.0, 0.0], "rust", 10, HYBRID).unwrap();
        assert_eq!(out[0].hit.text, "rust borrow checker");
        assert_eq!(out[0].match_sources, vec!["semantic", "bm25"]);
        let pipes = out
            .iter()
            .find(|h| h.hit.text == "rust in iron pipes")
            .unwrap();
        assert!(pipes.match_sources.contains(&"bm25"));
        let s0 = out[0].score.unwrap();
        assert!((0.0..=1.0).contains(&s0) && s0 > 0.99);
    }

    #[test]
    fn search_survives_queries_without_keywords() {
        let s = seeded();
        let out = search(&s, &[0.0, 1.0], "???", 2, HYBRID).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|h| h.match_sources == vec!["semantic"]));
    }

    #[test]
    fn search_serializes_to_the_ts_json_shape() {
        let s = seeded();
        let out = search(&s, &[1.0, 0.0], "rust", 1, HYBRID).unwrap();
        let v = serde_json::to_value(&out[0]).unwrap();
        for key in [
            "file_path",
            "heading",
            "context_path",
            "chunk_index",
            "text",
            "tags",
            "importance_score",
            "match_sources",
            "score",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }
}
