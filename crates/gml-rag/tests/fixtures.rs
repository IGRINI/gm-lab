//! Golden-fixture parity tests against `tests/reference/rag_*.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gml_config::Config;
use gml_rag::{
    decode_embedding_b64, encode_vector, retrieve_world_fact_with, tokens, EmbeddingCache, Embedder,
    HashEmbeddingClient, RagDocument, RagEngine,
};
use serde_json::Value;

fn reference_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("reference")
}

fn load_fixture(name: &str) -> Value {
    let path = reference_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("valid json fixture")
}

/// Build a Config whose RAG knobs exactly match the search fixture's `config`.
fn fixture_config() -> Config {
    let mut c = Config::from_env();
    let fx = load_fixture("rag_search.json");
    let cfg = &fx["config"];
    c.rag_enabled = true;
    c.rag_rrf_k = cfg["RRF_K"].as_i64().unwrap();
    c.rag_top_k = cfg["TOP_K"].as_i64().unwrap();
    c.rag_min_dense_score = cfg["MIN_DENSE_SCORE"].as_f64().unwrap();
    c.rag_keyword_tiebreak = cfg["KEYWORD_TIEBREAK"].as_f64().unwrap();
    c.rag_dense_tiebreak = cfg["DENSE_TIEBREAK"].as_f64().unwrap();
    c.rag_status_boost = cfg["STATUS_BOOST"].as_f64().unwrap();
    c.rag_fact_select_k = cfg["FACT_SELECT_K"].as_i64().unwrap();
    c
}

fn doc_from_json(v: &Value) -> RagDocument {
    let mut doc = RagDocument::new(
        v["doc_id"].as_str().unwrap(),
        v["kind"].as_str().unwrap(),
        v["text"].as_str().unwrap(),
    );
    if let Some(s) = v.get("status").and_then(|x| x.as_str()) {
        doc.status = s.to_string();
    }
    if let Some(s) = v.get("source").and_then(|x| x.as_str()) {
        doc.source = s.to_string();
    }
    if let Some(s) = v.get("visibility").and_then(|x| x.as_str()) {
        doc.visibility = s.to_string();
    }
    if let Some(arr) = v.get("tags").and_then(|x| x.as_array()) {
        doc.tags = arr.iter().map(|t| t.as_str().unwrap().to_string()).collect();
    }
    if let Some(obj) = v.get("metadata").and_then(|x| x.as_object()) {
        doc.metadata = obj.clone();
    }
    doc
}

fn search_docs() -> Vec<RagDocument> {
    let fx = load_fixture("rag_search.json");
    fx["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(doc_from_json)
        .collect()
}

#[test]
fn contextual_text_matches_fixture() {
    let expected = load_fixture("rag_contextual_text.json");
    let docs = search_docs();
    let by_id: BTreeMap<String, RagDocument> =
        docs.into_iter().map(|d| (d.doc_id.clone(), d)).collect();
    for (doc_id, want) in expected.as_object().unwrap() {
        let doc = by_id.get(doc_id).unwrap_or_else(|| panic!("doc {doc_id}"));
        assert_eq!(
            doc.contextual_text(),
            want.as_str().unwrap(),
            "contextual_text mismatch for {doc_id}"
        );
    }
}

#[test]
fn tokens_match_fixture() {
    let expected = load_fixture("rag_tokens.json");
    for (text, want) in expected.as_object().unwrap() {
        let got = tokens(text);
        let want_vec: Vec<String> = want
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want_vec, "tokens mismatch for {text:?}");
    }
}

#[test]
fn hash_embeddings_match_fixture() {
    let fx = load_fixture("rag_hash_embeddings.json");
    let dims = fx["dims"].as_u64().unwrap() as usize;
    let client = HashEmbeddingClient::new(dims);
    for (text, want) in fx["vectors"].as_object().unwrap() {
        let got = client.embed(&[text.clone()]).unwrap();
        let want_vec: Vec<f64> = want
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert_eq!(got[0].len(), want_vec.len(), "dims for {text:?}");
        for (i, (g, w)) in got[0].iter().zip(want_vec.iter()).enumerate() {
            assert!(
                (g - w).abs() <= 1e-6,
                "hash embedding mismatch for {text:?} at idx {i}: got {g}, want {w}"
            );
        }
    }
}

#[test]
fn search_rankings_match_fixture() {
    let config = fixture_config();
    let docs = search_docs();
    let engine = RagEngine::new(HashEmbeddingClient::new(128));
    let fx = load_fixture("rag_search.json");

    for (query, want_hits) in fx["rankings"].as_object().unwrap() {
        let hits = engine.search(query, &docs, None, &config).unwrap();
        let want = want_hits.as_array().unwrap();
        assert_eq!(
            hits.len(),
            want.len(),
            "hit count mismatch for query {query:?}: got {:?}",
            hits.iter().map(|h| &h.document.doc_id).collect::<Vec<_>>()
        );
        for (i, (hit, w)) in hits.iter().zip(want.iter()).enumerate() {
            assert_eq!(
                hit.document.doc_id,
                w["doc_id"].as_str().unwrap(),
                "doc_id order mismatch for {query:?} at rank {i}"
            );
            assert!(
                (hit.score - w["score"].as_f64().unwrap()).abs() <= 1e-6,
                "score mismatch {query:?} rank {i}: got {} want {}",
                hit.score,
                w["score"]
            );
            assert!(
                (hit.dense_score - w["dense"].as_f64().unwrap()).abs() <= 1e-6,
                "dense mismatch {query:?} rank {i}: got {} want {}",
                hit.dense_score,
                w["dense"]
            );
            assert!(
                (hit.keyword_score - w["keyword"].as_f64().unwrap()).abs() <= 1e-6,
                "keyword mismatch {query:?} rank {i}: got {} want {}",
                hit.keyword_score,
                w["keyword"]
            );
        }
    }
}

#[test]
fn vector_encode_decode_roundtrip_normalizes() {
    // encode is raw f32 LE; decode normalizes. Use an already-normalized vector
    // so the round-trip is identity within f32 precision.
    let v = vec![0.6_f64, 0.8_f64];
    let b64 = encode_vector(&v);
    let back = decode_embedding_b64(&b64).unwrap();
    assert_eq!(back.len(), 2);
    assert!((back[0] - 0.6).abs() < 1e-6);
    assert!((back[1] - 0.8).abs() < 1e-6);
}

#[test]
fn embedding_cache_put_get_delete_roundtrip() {
    let dir = std::env::temp_dir().join(format!("gml_rag_cache_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let db = dir.join("emb.sqlite3");
    let _ = std::fs::remove_file(&db);

    let cache = EmbeddingCache::new(&db).unwrap();
    let model = "test-model";
    let texts = vec!["hello world".to_string(), "second".to_string()];
    let v0 = vec![0.6_f64, 0.8_f64];
    let v1 = vec![1.0_f64, 0.0_f64];
    cache
        .put_many(model, &[(texts[0].clone(), v0.clone()), (texts[1].clone(), v1.clone())])
        .unwrap();

    let got = cache.get_many(model, &texts).unwrap();
    assert_eq!(got.len(), 2);
    let h0 = gml_rag::sha_text(&texts[0]);
    let back = got.get(&h0).unwrap();
    assert!((back[0] - 0.6).abs() < 1e-6 && (back[1] - 0.8).abs() < 1e-6);

    // delete by content hash removes across models
    let removed = cache.delete_by_text_hashes(&[h0.clone()]).unwrap();
    assert_eq!(removed, 1);
    let after = cache.get_many(model, &texts).unwrap();
    assert_eq!(after.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn retrieve_world_fact_matches_fixture() {
    let config = fixture_config();
    let docs = search_docs();
    let engine = RagEngine::new(HashEmbeddingClient::new(128));
    let fx = load_fixture("rag_search.json");

    for (query, want) in fx["retrieve_world_fact"].as_object().unwrap() {
        let got = retrieve_world_fact_with(&engine, query, &docs, &config).unwrap();
        if want.is_null() {
            assert!(got.is_none(), "expected None for {query:?}, got {got:?}");
            continue;
        }
        let got = got.unwrap_or_else(|| panic!("expected Some for {query:?}"));
        assert_eq!(
            got["status"], want["status"],
            "status mismatch for {query:?}"
        );
        assert_eq!(got["text"], want["text"], "text mismatch for {query:?}");
        let got_sources = got["sources"].as_array().unwrap();
        let want_sources = want["sources"].as_array().unwrap();
        assert_eq!(
            got_sources.len(),
            want_sources.len(),
            "sources len mismatch for {query:?}"
        );
        for (i, (g, w)) in got_sources.iter().zip(want_sources.iter()).enumerate() {
            // Compare structurally including rounded numeric scores.
            assert_eq!(g, w, "source {i} mismatch for {query:?}");
        }
    }
}
