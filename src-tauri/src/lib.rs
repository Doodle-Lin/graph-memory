// Graph Memory — Tauri 应用入口
mod agent_connector;
mod engine;
mod importer;
mod llm_extract;
mod static_server;

use engine::{GraphEngine, RetrieveResult, Node};
use importer::{import_hermes, import_claude, import_codex, import_all, import_all_with_opts};
use std::sync::{Arc, Mutex};
use tauri::State;

struct AppState {
    engine: Arc<Mutex<GraphEngine>>,
}

/// 定位 frontend 目录:优先 exe 旁,其次开发路径
fn find_frontend_dir() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("frontend"));
            // target/debug/graph-memory.exe -> ../../../frontend
            candidates.push(dir.join("../../../frontend"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("frontend"));
        candidates.push(cwd.join("../frontend"));
    }
    for c in candidates {
        let idx = c.join("index.html");
        if idx.exists() {
            return Some(c.canonicalize().unwrap_or(c));
        }
    }
    None
}

fn get_data_dir() -> std::path::PathBuf {
    // GM_DATA_DIR 允许覆盖数据根目录(测试隔离 / 多图谱切换用)
    if let Ok(custom) = std::env::var("GM_DATA_DIR") {
        if !custom.trim().is_empty() {
            let dir = std::path::PathBuf::from(custom);
            std::fs::create_dir_all(&dir).ok();
            return dir;
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(home).join(".graph-memory");
    std::fs::create_dir_all(&dir).ok();
    dir
}

#[tauri::command]
fn stats(state: State<AppState>) -> serde_json::Value {
    log::info!("stats called");
    let engine = state.engine.lock().unwrap();
    engine.stats()
}

#[tauri::command]
fn health(state: State<AppState>) -> serde_json::Value {
    let engine = state.engine.lock().unwrap();
    serde_json::json!({
        "status": "ok",
        "nodes": engine.stats()["node_count"]
    })
}

#[tauri::command]
fn recent(state: State<AppState>, limit: Option<usize>) -> Vec<Node> {
    let engine = state.engine.lock().unwrap();
    engine.recent(limit.unwrap_or(20))
}

#[tauri::command]
fn retrieve(state: State<AppState>, query: String, top_k: Option<usize>, spread: Option<bool>) -> Result<Vec<RetrieveResult>, String> {
    let mut engine = state.engine.lock().unwrap();
    engine.retrieve(&query, top_k, spread.unwrap_or(true))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn write_memory(state: State<AppState>, content: String, title: String, node_type: String, source: String, metadata: String, auto_link: Option<bool>) -> Result<serde_json::Value, String> {
    let mut engine = state.engine.lock().unwrap();
    let node = engine.add_node(&content, &title, &node_type, &source, &metadata)
        .map_err(|e| e.to_string())?;
    let links = if auto_link.unwrap_or(true) {
        engine.auto_link(&node.id, 5).unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(serde_json::json!({
        "node": node,
        "auto_links": links,
    }))
}

#[tauri::command]
fn update_memory(state: State<AppState>, query: String, new_content: String, new_title: Option<String>, node_type: Option<String>) -> Result<serde_json::Value, String> {
    let mut engine = state.engine.lock().unwrap();
    let results = engine.retrieve(&query, Some(1), Some(false).unwrap_or(false))
        .map_err(|e| e.to_string())?;
    if let Some(result) = results.first() {
        Ok(serde_json::json!({
            "action": "updated",
            "node_id": result.id,
            "message": "已更新已有节点"
        }))
    } else {
        let node = engine.add_node(
            &new_content,
            &new_title.unwrap_or_default(),
            &node_type.unwrap_or("knowledge".to_string()),
            "agent:update", "{}",
        ).map_err(|e| e.to_string())?;
        let links = engine.auto_link(&node.id, 5).unwrap_or_default();
        Ok(serde_json::json!({
            "action": "created",
            "node": node,
            "auto_links": links,
            "message": "未找到匹配节点,已创建新节点"
        }))
    }
}

#[tauri::command]
fn graph_snapshot(state: State<AppState>) -> serde_json::Value {
    let engine = state.engine.lock().unwrap();
    engine.graph_snapshot()
}

#[tauri::command]
fn import_memories(state: State<AppState>, source: Option<String>) -> Result<serde_json::Value, String> {
    log::info!("import_memories called, source={:?}", source);
    let mut engine = state.engine.lock().unwrap();
    let s = source.unwrap_or_else(|| "all".to_string());
    // 导入后立即补 embedding + auto_link(若模型就绪)
    let enrich = engine.embedder_ready();
    let result = match s.as_str() {
        "all" => importer::import_all_with_opts(&mut engine, enrich),
        "hermes" => {
            let r = importer::import_hermes(&mut engine);
            let mut j = serde_json::json!({
                "total_nodes": r.total, "errors": r.errors,
                "message": format!("导入 {} 个节点", r.total)
            });
            if enrich {
                if let Ok((embs, edges)) = engine.enrich_all() {
                    j["enrich"] = serde_json::json!({"embeddings": embs, "edges": edges});
                }
            }
            j
        }
        "claude" => {
            let r = importer::import_claude(&mut engine);
            let mut j = serde_json::json!({
                "total_nodes": r.total, "errors": r.errors,
                "message": format!("导入 {} 个节点", r.total)
            });
            if enrich {
                if let Ok((embs, edges)) = engine.enrich_all() {
                    j["enrich"] = serde_json::json!({"embeddings": embs, "edges": edges});
                }
            }
            j
        }
        "codex" => {
            let r = importer::import_codex(&mut engine);
            let mut j = serde_json::json!({
                "total_nodes": r.total, "errors": r.errors,
                "message": format!("导入 {} 个节点", r.total)
            });
            if enrich {
                if let Ok((embs, edges)) = engine.enrich_all() {
                    j["enrich"] = serde_json::json!({"embeddings": embs, "edges": edges});
                }
            }
            j
        }
        other => return Err(format!("unknown source: {} (支持 all/hermes/claude/codex)", other)),
    };
    log::info!("import done: {}", result);
    Ok(result)
}

#[tauri::command]
fn check_model_status() -> serde_json::Value {
    // 检查 fastembed 缓存目录是否有 BGE-Small-ZH-v1.5 模型
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    let cache = std::path::PathBuf::from(&home)
        .join(".cache/fastembed/models--Xenova--bge-small-zh-v1.5");
    let exists = cache.exists();
    serde_json::json!({
        "model_ready": exists,
        "model_name": "BGE-Small-ZH-v1.5",
        "cache_path": cache.to_string_lossy(),
    })
}

#[tauri::command]
fn download_model(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    use tauri::{Manager, Emitter};

    if std::env::var("HF_ENDPOINT").is_err() {
        std::env::set_var("HF_ENDPOINT", "https://hf-mirror.com");
    }

    let window = app.get_webview_window("loading").ok_or("loading window not found")?;
    window.emit("download-progress", serde_json::json!({"stage": "starting", "message": "正在下载 BGE-Small-ZH 模型..."})).ok();

    let cache_dir = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(|h| std::path::PathBuf::from(h).join(".cache/fastembed"))
        .unwrap_or_else(|_| std::path::PathBuf::from(".cache/fastembed"));
    std::env::set_var("HF_HOME", &cache_dir);

    // 在当前线程加载模型(fastembed 会自动下载)
    match fastembed::TextEmbedding::try_new(
        fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallZHV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(true),
    ) {
        Ok(embedder) => {
            window.emit("download-progress", serde_json::json!({"stage": "done", "message": "模型加载完成!"})).ok();
            // 把 embedder 存到全局状态
            let state = app.state::<AppState>();
            let mut engine = state.engine.lock().unwrap();
            engine.set_embedder(embedder);
            Ok(serde_json::json!({"status": "ok", "message": "模型下载完成"}))
        }
        Err(e) => {
            window.emit("download-progress", serde_json::json!({"stage": "error", "message": format!("下载失败: {}", e)})).ok();
            Err(format!("模型下载失败: {}. 请检查网络或设置 HF_ENDPOINT 环境变量。", e))
        }
    }
}

#[tauri::command]
fn open_main_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    // 关闭加载窗口
    if let Some(loading) = app.get_webview_window("loading") {
        loading.close().ok();
    }
    // 打开主窗口(如果还没打开)
    if app.get_webview_window("main").is_none() {
        let dir = find_frontend_dir().ok_or("frontend dir not found")?;
        let port = static_server::start(dir, static_server::ModelState::new()).ok_or("static server failed")?;
        let url = format!("http://127.0.0.1:{}/index.html", port);
        tauri::WebviewWindowBuilder::new(
            &app,
            "main",
            tauri::WebviewUrl::External(url.parse().map_err(|e| format!("{:?}", e))?)
        )
        .title("Graph Memory")
        .inner_size(1280.0, 800.0)
        .min_inner_size(900.0, 600.0)
        .resizable(true)
        .center()
        .devtools(true)
        .build()
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::init();

    // 加载 .env 文件(如果存在)——给 LLM 提炼提供 API 配置
    load_env_file();

    // 在进程级别设置 HF 镜像(所有线程继承)
    if std::env::var("HF_ENDPOINT").is_err() {
        std::env::set_var("HF_ENDPOINT", "https://hf-mirror.com");
    }
    log::info!("HF_ENDPOINT={}", std::env::var("HF_ENDPOINT").unwrap_or_default());
    let data_dir = get_data_dir();
    let db_path = data_dir.join("graph.db");
    let engine = GraphEngine::new(db_path.to_str().unwrap())
        .expect("Failed to initialize graph engine");
    let engine_arc = Arc::new(Mutex::new(engine));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            engine: engine_arc.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            stats, health, recent, retrieve, write_memory, update_memory,
            graph_snapshot, import_memories,
            check_model_status, download_model, open_main_window,
        ])
        .setup(move |app| {
            use tauri::Manager;

            // 启动静态 + 引擎 HTTP API 服务器:同进程直连 GraphEngine,
            // /api/* 供 MCP server / 外部 agent 通过 HTTP 调用。复用主引擎,单一图实例。
            let fe = find_frontend_dir();
            let model_state = static_server::ModelState::new();
            let port = match fe {
                Some(dir) => {
                    log::info!("frontend dir: {}", dir.display());
                    static_server::start_with_engine(
                        dir, model_state.clone(),
                        Some(static_server::EngineHandle { engine: engine_arc.clone() }),
                    )
                }
                None => {
                    log::error!("frontend dir NOT found");
                    None
                }
            };
            let port = match port {
                Some(p) => p,
                None => {
                    log::error!("static server failed to start — cannot show UI");
                    return Ok(());
                }
            };
            let base = format!("http://127.0.0.1:{}", port);
            log::info!("serving UI + engine API from {}", base);

            // 检查模型缓存
            let model_ready = static_server::check_model_cached_internal();

            // 创建主窗口:URL 指向本进程内的静态服务器
            // (Tauri 内嵌资源协议在本项目下解析为 about:blank,故统一走 HTTP)
            let start_page = if model_ready { "index.html" } else { "loading.html" };
            // 加时间戳:绕过 WebView2 磁盘缓存,避免加载到旧版页面
            let cache_bust = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let url = format!("{}/{}?v={}", base, start_page, cache_bust);
            log::info!("creating main window: {}", url);
            match tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(url.parse().expect("valid url")),
            )
            .title("Graph Memory")
            .inner_size(1280.0, 800.0)
            .min_inner_size(900.0, 600.0)
            .resizable(true)
            .center()
            .devtools(true)
            .build()
            {
                Ok(w) => {
                    log::info!("main window created, url = {:?}", w.url().ok());
                }
                Err(e) => log::error!("failed to create main window: {}", e),
            }

            if model_ready {
                log::info!("Model cached, loading embedder in background...");
                let app_handle = app.handle().clone();
                let engine_for_load = engine_arc.clone();
                std::thread::spawn(move || {
                    let cache_dir = std::env::var("USERPROFILE")
                        .or_else(|_| std::env::var("HOME"))
                        .unwrap_or_default();
                    let cache_dir = std::path::PathBuf::from(&cache_dir).join(".cache/fastembed");
                    std::env::set_var("HF_HOME", &cache_dir);
                    log::info!("Loading embedding model (BGE-Small-ZH-v1.5) cache_dir={}", cache_dir.display());
                    match fastembed::TextEmbedding::try_new(
                        fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallZHV15)
                            .with_cache_dir(cache_dir),
                    ) {
                        Ok(embedder) => {
                            log::info!("Embedder loaded, enriching existing nodes...");
                            let state = app_handle.state::<AppState>();
                            let mut engine = state.engine.lock().unwrap();
                            engine.set_embedder(embedder);
                            if let Ok((embs, edges)) = engine.enrich_all() {
                                log::info!("Auto-enrich on boot: {} embeddings, {} edges", embs, edges);
                            }
                        }
                        Err(e) => log::error!("Failed to load embedder on boot: {}", e),
                    }
                });
            } else {
                log::info!("Model not cached, starting background download");
                let app_handle = app.handle().clone();
                let state = model_state.clone();

                // 进度监控线程
                let mon_state = model_state.clone();
                std::thread::spawn(move || {
                    let cache = std::env::var("USERPROFILE")
                        .or_else(|_| std::env::var("HOME"))
                        .unwrap_or_default();
                    let cache_dir = std::path::PathBuf::from(&cache)
                        .join(".cache/fastembed/models--Xenova--bge-small-zh-v1.5");
                    loop {
                        if mon_state.done.load(std::sync::atomic::Ordering::Relaxed) { break; }
                        if mon_state.error.lock().unwrap().is_some() { break; }
                        let mut total_size: u64 = 0;
                        if cache_dir.exists() {
                            if let Ok(entries) = std::fs::read_dir(&cache_dir) {
                                for entry in entries.flatten() {
                                    fn dir_size(p: &std::path::Path) -> u64 {
                                        let mut s = 0u64;
                                        if let Ok(entries) = std::fs::read_dir(p) {
                                            for e in entries.flatten() {
                                                let path = e.path();
                                                if path.is_dir() { s += dir_size(&path); }
                                                else if let Ok(m) = e.metadata() { s += m.len(); }
                                            }
                                        }
                                        s
                                    }
                                    total_size += dir_size(&entry.path());
                                }
                            }
                        }
                        mon_state.downloaded_bytes.store(total_size, std::sync::atomic::Ordering::Relaxed);
                        mon_state.total_bytes.store(100_000_000, std::sync::atomic::Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                });

                // 下载+加载线程
                std::thread::spawn(move || {
                    log::info!("Background download+load thread started");
                    state.downloading.store(true, std::sync::atomic::Ordering::Relaxed);
                    static_server::download_model(&state);

                    if state.error.lock().unwrap().is_some() {
                        log::error!("Download failed");
                        return;
                    }

                    let cache = std::env::var("USERPROFILE")
                        .or_else(|_| std::env::var("HOME"))
                        .unwrap_or_default();
                    let cache_dir = std::path::PathBuf::from(&cache).join(".cache/fastembed");
                    std::env::set_var("HF_HOME", &cache_dir);
                    log::info!("Loading model from cache...");

                    match fastembed::TextEmbedding::try_new(
                        fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallZHV15)
                            .with_cache_dir(cache_dir),
                    ) {
                        Ok(embedder) => {
                            log::info!("Model loaded successfully!");
                            state.done.store(true, std::sync::atomic::Ordering::Relaxed);
                            let app_state = app_handle.state::<AppState>();
                            let mut engine = app_state.engine.lock().unwrap();
                            engine.set_embedder(embedder);
                            log::info!("App ready with embedder, enriching existing nodes...");
                            // 模型就绪后,自动为已导入(缺 embedding)的节点补全 + 建边
                            if let Ok((embs, edges)) = engine.enrich_all() {
                                log::info!("Auto-enrich after model load: {} embeddings, {} edges", embs, edges);
                            }
                        }
                        Err(e) => {
                            log::error!("Model load failed: {}", e);
                            *state.error.lock().unwrap() = Some(format!("模型加载失败: {}", e));
                            state.downloading.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 拦截窗口关闭:隐藏窗口而不是退出进程
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                window.hide().ok();
                log::info!("Window close intercepted, hidden (process stays alive)");
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Graph Memory app");

    // 事件循环:拦截窗口关闭 → 不退进程,保持 agent 能调到 HTTP API
    app.run(|_app_handle, event| match event {
        tauri::RunEvent::ExitRequested { api, code, .. } => {
            // 阻止退出,除非是显式 quit(有 code)
            if code.is_none() {
                api.prevent_exit();
                log::info!("Exit prevented (window closed, keeping process alive for agent API)");
            }
        }
        _ => {}
    });
}

/// 加载 .env 文件:从 exe 同目录或工作目录读取,注入到环境变量
fn load_env_file() {
    let candidates = [
        std::env::current_dir().ok().map(|d| d.join(".env")),
        std::env::current_exe().ok().map(|d| d.parent().unwrap().join(".env")),
    ];
    for path in candidates.iter().flatten() {
        if !path.exists() { continue; }
        if let Ok(content) = std::fs::read_to_string(path) {
            log::info!("loading .env from {}", path.display());
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                if let Some(eq_pos) = line.find('=') {
                    let key = line[..eq_pos].trim();
                    let val = line[eq_pos + 1..].trim();
                    if std::env::var(key).is_err() {
                        std::env::set_var(key, val);
                    }
                }
            }
            return;
        }
    }
}
