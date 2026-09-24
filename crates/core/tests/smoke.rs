//! Real-vault smoke run with the fast test embedder:
//!   ANAMNESIS_SMOKE_DIR=/path/to/vault cargo test -p anamnesis-core --test smoke -- --ignored --nocapture

use anamnesis_core::config::Config;
use anamnesis_core::embed::HashEmbedder;
use anamnesis_core::indexer::Indexer;
use anamnesis_core::store::Store;
use std::sync::{Arc, RwLock};

#[test]
#[ignore = "needs ANAMNESIS_SMOKE_DIR"]
fn index_a_real_vault_twice() {
    let dir = std::env::var("ANAMNESIS_SMOKE_DIR").expect("set ANAMNESIS_SMOKE_DIR");
    let tmp = tempfile::tempdir().unwrap();
    let (store, _) = Store::open(&tmp.path().join("a.db"), "hash", 64).unwrap();
    let store = Arc::new(store);
    let cfg = Arc::new(RwLock::new(Config {
        watch_dirs: vec![dir],
        ..Config::default()
    }));
    let idx = Indexer::new(store.clone(), Arc::new(HashEmbedder::new(64)), cfg);
    let t = std::time::Instant::now();
    let first = idx.sync(false).unwrap();
    println!("first: {first:?} in {:?}", t.elapsed());
    let second = idx.sync(false).unwrap();
    println!("second: {second:?}");
    assert_eq!(second.indexed, 0);
    let forced = idx.sync(true).unwrap();
    println!(
        "forced: {forced:?}, chunks {}",
        store.chunk_count().unwrap()
    );
}
