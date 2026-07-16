//! RAG Phase A (per-world cache isolation) tests — hermetic.
//!
//! Every test sets `GM_RAG_WORLDS_DIR` + `GM_RAG_CACHE_PATH` to tempdirs (or
//! builds a `Config` by hand) so nothing touches the real user library. The
//! routing tests need cache-through, which lives only in `LocalEmbeddingClient`,
//! so they stand up a tiny deterministic HTTP embedding server (a raw
//! `TcpListener` — no extra deps) and observe WHICH `.sqlite3` file the document
//! vectors land in.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use gml_config::Config;
use gml_rag::{
    delete_world_cache, purge_embeddings_for_texts, resolve_cache_path, retrieve_world_fact_at,
    with_engine_at, world_cache_path, EmbeddingCache, RagDocument,
};

/// Minimal deterministic embeddings HTTP server. Answers any POST with a JSON
/// body `{ "input": [..] }` by returning a fixed 2-dim vector per input. Enough
/// to exercise `LocalEmbeddingClient::embed` (documents, cached) +
/// `embed_query`; the vectors are content-independent because these tests care
/// about cache ROUTING, not ranking. Returns the base URL and a stop handle.
struct MockEmbedServer {
    url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockEmbedServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock embed server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/v1/embeddings");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_nonblocking(false);
                        // Read the request (headers + body). We only need the
                        // count of inputs; parse the JSON body after the blank line.
                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 4096];
                        // Read until we have headers + full body (Content-Length).
                        loop {
                            match sock.read(&mut tmp) {
                                Ok(0) => break,
                                Ok(n) => {
                                    buf.extend_from_slice(&tmp[..n]);
                                    if let Some(body) = split_body(&buf) {
                                        if body_complete(&buf, body) {
                                            break;
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let n_inputs = count_inputs(&buf);
                        let mut data = String::from("[");
                        for i in 0..n_inputs.max(1) {
                            if i > 0 {
                                data.push(',');
                            }
                            // A tiny non-degenerate vector; identical per input is
                            // fine (routing test, not ranking).
                            data.push_str(&format!("{{\"index\":{i},\"embedding\":[0.6,0.8]}}"));
                        }
                        data.push(']');
                        let body = format!("{{\"data\":{data}}}");
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = sock.write_all(resp.as_bytes());
                        let _ = sock.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        MockEmbedServer {
            url,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MockEmbedServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn split_body(buf: &[u8]) -> Option<usize> {
    // Return byte offset where the body starts (after \r\n\r\n).
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn body_complete(buf: &[u8], body_start: usize) -> bool {
    let headers = &buf[..body_start];
    let text = String::from_utf8_lossy(headers).to_lowercase();
    if let Some(idx) = text.find("content-length:") {
        let rest = &text[idx + "content-length:".len()..];
        let num: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(len) = num.parse::<usize>() {
            return buf.len() - body_start >= len;
        }
    }
    // No content-length: assume complete once we saw the blank line.
    true
}

fn count_inputs(buf: &[u8]) -> usize {
    let Some(body_start) = split_body(buf) else {
        return 1;
    };
    let body = String::from_utf8_lossy(&buf[body_start..]);
    let value: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return 1,
    };
    value
        .get("input")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(1)
}

/// A `Config` wired to `server` for embeddings, with both cache locations under
/// `dir` (global file + per-world dir), RAG on, rerank off.
fn hermetic_config(dir: &Path, server: &MockEmbedServer) -> Config {
    let mut c = Config::from_env();
    c.rag_enabled = true;
    c.rag_rerank_enabled = false;
    c.rag_min_dense_score = -1.0;
    c.rag_top_k = 4;
    c.rag_fact_select_k = 4;
    c.rag_timeout_seconds = 5.0;
    c.rag_embeddings_url = server.url.clone();
    c.rag_cache_path = dir.join("global.sqlite3").to_string_lossy().into_owned();
    c.rag_worlds_dir = dir.join("rag_worlds").to_string_lossy().into_owned();
    c
}

fn sample_docs() -> Vec<RagDocument> {
    vec![
        RagDocument::new("d1", "fact", "the north gate password is winter"),
        RagDocument::new("d2", "fact", "the harbor master keeps a ledger"),
    ]
}

// ---------------------------------------------------------------------------

#[test]
fn world_cache_path_sanitizes_and_falls_back() {
    let mut c = Config::from_env();
    c.rag_worlds_dir = "/tmp/rag_worlds".to_string();

    // Safe ids: used verbatim as the stem.
    let p = world_cache_path(&c, "abc-DEF_123");
    assert_eq!(
        p.file_name().unwrap().to_string_lossy(),
        "abc-DEF_123.sqlite3"
    );

    // Unsafe ids: deterministic `w-<32 hex>` fallback, stable across calls.
    let p1 = world_cache_path(&c, "world/with space");
    let p2 = world_cache_path(&c, "world/with space");
    let stem1 = p1.file_stem().unwrap().to_string_lossy().into_owned();
    assert_eq!(p1, p2, "fallback must be deterministic");
    assert!(stem1.starts_with("w-"), "fallback stem: {stem1}");
    assert_eq!(stem1.len(), 2 + 32, "w- + 128-bit hex prefix");
    assert!(
        stem1[2..].chars().all(|c| c.is_ascii_hexdigit()),
        "hex only: {stem1}"
    );

    // Distinct unsafe ids -> distinct fallback stems.
    let other = world_cache_path(&c, "другой мир");
    assert_ne!(p1.file_stem(), other.file_stem());

    // Empty id -> fallback (never an empty stem escaping the dir).
    let empty = world_cache_path(&c, "");
    assert!(empty
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .starts_with("w-"));

    // Unicode/path chars never leak a separator into the stem.
    let stem_full = p1.file_name().unwrap().to_string_lossy();
    assert!(!stem_full.contains('/') && !stem_full.contains('\\'));
}

#[test]
fn resolve_cache_path_routes_by_world_id() {
    let mut c = Config::from_env();
    c.rag_cache_path = "/data/global.sqlite3".to_string();
    c.rag_worlds_dir = "/data/rag_worlds".to_string();
    assert_eq!(
        resolve_cache_path(&c, None),
        PathBuf::from("/data/global.sqlite3")
    );
    assert_eq!(
        resolve_cache_path(&c, Some("w1")),
        PathBuf::from("/data/rag_worlds").join("w1.sqlite3")
    );
    // Empty / whitespace ids are the global sentinel (same as None), so writes
    // and purge agree for a blank id (RAG_PER_WORLD_TZ §2.2).
    let global = PathBuf::from("/data/global.sqlite3");
    assert_eq!(resolve_cache_path(&c, Some("")), global);
    assert_eq!(resolve_cache_path(&c, Some("  ")), global);
    assert_eq!(resolve_cache_path(&c, Some("\t\n")), global);
}

#[test]
fn world_bound_retrieval_writes_only_the_per_world_file() {
    let server = MockEmbedServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = hermetic_config(dir.path(), &server);
    let docs = sample_docs();

    let world_id = "world-alpha";
    let per_world = world_cache_path(&config, world_id);
    let global = PathBuf::from(&config.rag_cache_path);

    let out = retrieve_world_fact_at(&per_world, "north gate password", &docs, &config)
        .expect("retrieval ok");
    assert!(out.is_some(), "expected a hit from the mock embedder");

    assert!(per_world.is_file(), "per-world cache must be created");
    assert!(
        !global.is_file(),
        "global cache must be untouched for a world-bound retrieval"
    );

    // The per-world file actually holds the document vectors.
    let cache = EmbeddingCache::new(&per_world).expect("open per-world cache");
    let model = format!("{}@{}", config.rag_embeddings_model, config.embedder_quant);
    let texts: Vec<String> = docs.iter().map(|d| d.contextual_text()).collect();
    let found = cache.get_many(&model, &texts).expect("get_many");
    assert_eq!(found.len(), docs.len(), "all docs cached in per-world file");
}

#[test]
fn world_ref_none_writes_the_global_file() {
    let server = MockEmbedServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = hermetic_config(dir.path(), &server);
    let docs = sample_docs();

    let global = resolve_cache_path(&config, None);
    let out = retrieve_world_fact_at(&global, "north gate password", &docs, &config).expect("ok");
    assert!(out.is_some());

    assert!(global.is_file(), "global cache created for None world");
    // No stray per-world files got written.
    let worlds_dir = Path::new(&config.rag_worlds_dir);
    let leaked = worlds_dir.exists()
        && std::fs::read_dir(worlds_dir)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
    assert!(!leaked, "no per-world file for a None-world retrieval");
}

#[test]
fn registry_shares_one_engine_per_world_and_separates_worlds() {
    let server = MockEmbedServer::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = hermetic_config(dir.path(), &server);

    let path_a = world_cache_path(&config, "reg-a");
    let path_b = world_cache_path(&config, "reg-b");

    // Two calls for the same world -> same engine instance (pointer identity of
    // the embedder's cache, captured via a raw pointer through the closure).
    let ptr1 = with_engine_at(&config, &path_a, |e| {
        Ok(std::ptr::addr_of!(e.embedder) as usize)
    })
    .expect("engine a #1");
    let ptr2 = with_engine_at(&config, &path_a, |e| {
        Ok(std::ptr::addr_of!(e.embedder) as usize)
    })
    .expect("engine a #2");
    assert_eq!(ptr1, ptr2, "same world -> one cached engine instance");

    // Different world -> different engine instance.
    let ptr_b = with_engine_at(&config, &path_b, |e| {
        Ok(std::ptr::addr_of!(e.embedder) as usize)
    })
    .expect("engine b");
    assert_ne!(ptr1, ptr_b, "different worlds -> different engines");

    // Exercise the two engines so each writes its own file.
    let docs = sample_docs();
    let _ = retrieve_world_fact_at(&path_a, "gate", &docs, &config);
    let _ = retrieve_world_fact_at(&path_b, "gate", &docs, &config);
    assert!(path_a.is_file());
    assert!(path_b.is_file());
    assert_ne!(path_a, path_b);
}

#[test]
fn delete_world_cache_removes_file_and_sidecars_and_is_noop_when_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::from_env();
    config.rag_worlds_dir = dir.path().join("rag_worlds").to_string_lossy().into_owned();
    let world_id = "gc-world";

    // Missing file -> no-op, returns false, never errors.
    assert!(!delete_world_cache(&config, world_id));

    // Create the file + sidecars, then GC.
    let main = world_cache_path(&config, world_id);
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::write(&main, b"db").unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut s = main.as_os_str().to_os_string();
        s.push(suffix);
        std::fs::write(PathBuf::from(s), b"x").unwrap();
    }
    assert!(delete_world_cache(&config, world_id), "main file existed");
    assert!(!main.is_file());
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut s = main.as_os_str().to_os_string();
        s.push(suffix);
        assert!(!PathBuf::from(s).is_file(), "sidecar {suffix} removed");
    }
}

#[test]
fn scoped_purge_only_touches_the_target_world_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::from_env();
    config.rag_enabled = true;
    config.rag_cache_path = dir
        .path()
        .join("global.sqlite3")
        .to_string_lossy()
        .into_owned();
    config.rag_worlds_dir = dir.path().join("rag_worlds").to_string_lossy().into_owned();

    let model = format!("{}@{}", config.rag_embeddings_model, config.embedder_quant);
    let text = "shared session text about the north gate".to_string();
    let vec = vec![0.6_f64, 0.8_f64];

    // Seed the SAME text into world-a, world-b, and the global cache.
    let path_a = world_cache_path(&config, "purge-a");
    let path_b = world_cache_path(&config, "purge-b");
    let path_g = PathBuf::from(&config.rag_cache_path);
    for p in [&path_a, &path_b, &path_g] {
        let cache = EmbeddingCache::new(p).unwrap();
        cache
            .put_many(&model, &[(text.clone(), vec.clone())])
            .unwrap();
    }

    // Purge scoped to world-a only.
    let removed = purge_embeddings_for_texts(std::slice::from_ref(&text), &config, Some("purge-a"));
    assert_eq!(removed, 1, "one row removed from world-a");

    let gone_a = EmbeddingCache::new(&path_a)
        .unwrap()
        .get_many(&model, std::slice::from_ref(&text))
        .unwrap();
    assert!(gone_a.is_empty(), "world-a text purged");
    let kept_b = EmbeddingCache::new(&path_b)
        .unwrap()
        .get_many(&model, std::slice::from_ref(&text))
        .unwrap();
    assert_eq!(kept_b.len(), 1, "world-b text untouched");
    let kept_g = EmbeddingCache::new(&path_g)
        .unwrap()
        .get_many(&model, std::slice::from_ref(&text))
        .unwrap();
    assert_eq!(kept_g.len(), 1, "global text untouched");

    // None scope -> purges the global file.
    let removed_g = purge_embeddings_for_texts(std::slice::from_ref(&text), &config, None);
    assert_eq!(removed_g, 1);
    let gone_g = EmbeddingCache::new(&path_g)
        .unwrap()
        .get_many(&model, std::slice::from_ref(&text))
        .unwrap();
    assert!(gone_g.is_empty(), "global text purged for None scope");
}
