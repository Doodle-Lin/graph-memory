//! Experimental headless runtime with an explicitly selected data directory.
use crate::{
    engine::GraphEngine,
    static_server::{self, EngineHandle, ModelState},
};
use anyhow::{Context, Result};
use std::{
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

pub fn run(data_dir: &Path, port: u16) -> Result<()> {
    std::fs::create_dir_all(data_dir).context("create service data directory")?;
    let data_dir = data_dir
        .canonicalize()
        .context("resolve service data directory")?;
    // SQLite holds an OS-backed exclusive lock, released on exit or crash.
    let guard = rusqlite::Connection::open(data_dir.join("service-lock.sqlite"))?;
    guard.busy_timeout(Duration::ZERO)?;
    guard
        .execute_batch("BEGIN EXCLUSIVE;")
        .context("another service owns this data directory")?;
    // Never open the legacy desktop database during this experimental stage.
    let engine = GraphEngine::new(
        data_dir
            .join("service-graph.db")
            .to_str()
            .context("data directory is not UTF-8")?,
    )?;
    let listener = TcpListener::bind(("127.0.0.1", port)).context("bind local service port")?;
    let port = listener.local_addr()?.port();
    let info = serde_json::json!({"protocol_version":1,"instance_id":uuid::Uuid::new_v4().to_string(),
        "pid":std::process::id(),"data_dir":data_dir,"api_url":format!("http://127.0.0.1:{port}"),
        "mode":"keyword-only","experimental":true});
    let temporary = data_dir.join("service.json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&info)?)?;
    std::fs::rename(&temporary, data_dir.join("service.json")).context("publish discovery")?;
    let mut state = ModelState::new();
    state.service_info = Some(Arc::new(info.clone()));
    state.memory_store = Some(Arc::new(Mutex::new(
        crate::memory_store::MemoryStore::open(&data_dir.join("work-memory.db"))
            .map_err(|e| anyhow::anyhow!("{}", e.1))?,
    )));
    println!("{}", info);
    let result = static_server::serve_api(
        listener,
        state,
        EngineHandle {
            engine: Arc::new(Mutex::new(engine)),
        },
    );
    drop(guard);
    result.context("service listener stopped")
}
