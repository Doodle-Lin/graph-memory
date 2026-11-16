//! P5 bounded semantic recall: offline model management + embeddings for
//! `work-memory.db`'s `memory_vectors` table.
//!
//! Contract: `docs/design/P5_SEMANTIC_CONTRACT.md` (authority).
//!
//! Design notes
//! * The retrieval path is **offline**: it only ever reads a locally verified model
//!   directory. Downloading happens exclusively through [`SemanticRuntime::prepare`]
//!   (explicit trigger) and never on startup or on a search.
//! * Download progress is reported from real measured bytes (`Content-Length` and the
//!   running byte counter); nothing here fabricates a percentage.
//! * The mirror `hf-mirror.com` does not honour HTTP Range, which makes `hf-hub`'s
//!   metadata probe fail with "Header Content-Range is missing". We therefore use an
//!   explicit URL list plus per-file size + SHA256 verification instead of relying on
//!   `hf-hub`'s downloader (`fastembed` still loads the files through `hf-hub`'s cache
//!   layout, fully offline).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Model identifier recorded in `memory_vectors.model` and exposed over HTTP.
pub const MODEL_ID: &str = "bge-small-zh-v1.5";
/// Upstream repository (both mirror and official endpoints serve it).
pub const MODEL_REPO: &str = "Xenova/bge-small-zh-v1.5";
/// Pinned revision: the manifest below is only valid for this commit.
pub const MODEL_REVISION: &str = "75c43b069aac4d136ba6bc1122f995fedcfd2781";
/// `hf-hub` folder name for the repository above.
pub const MODEL_DIR_NAME: &str = "models--Xenova--bge-small-zh-v1.5";
/// Embedding dimension of `MODEL_ID` (BGE-small-zh-v1.5 is 512, not 384).
pub const MODEL_DIM: usize = 512;
/// Contract §3.1 default (v1.4: 0.52). Chosen from the **query-vs-record** distribution:
/// the 30 off-topic probes top out at cosine 0.5160 and official `no_answer` at 0.4647, so
/// 0.52 clears both while yielding the same correct cross-literal suggestions as 0.42.
/// See `docs/design/P5_SEMANTIC_CONTRACT.md` §6.1 evidence C.
pub const DEFAULT_MIN_SIMILARITY: f64 = 0.52;
/// Contract §3.1 upper bound for `semantic_limit`.
pub const MAX_SEMANTIC_LIMIT: usize = 10;
/// Contract §2.1 v1.5: how long a `semantic=true` request may wait for the background
/// warm-up to finish building the ONNX session before degrading. Bounded waiting is what
/// keeps the channel deterministic: a request never sees "degraded now, hybrid 3 s later".
pub const SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
/// Contract §5 default location: `%LOCALAPPDATA%\GraphMemory\models`.
pub const ENV_MODEL_DIR: &str = "GM_EMBED_MODEL_DIR";
/// Env override for the minimum similarity (used by tests/verification).
pub const ENV_MIN_SIMILARITY: &str = "GM_SEMANTIC_MIN_SIMILARITY";

const VERIFIED_MARKER: &str = ".gm-verified";

/// Manifest entry: relative path inside the repo, byte size, SHA256, upstream git-blob etag.
struct FileSpec {
    path: &'static str,
    size: u64,
    sha256: &'static str,
    etag: &'static str,
}

/// Authoritative manifest for [`MODEL_REVISION`].
///
/// `size`/`etag` were read from the mirror's `Content-Length`/`X-Linked-Etag` headers on
/// 2026-11-05; `sha256` was computed from a local copy whose GitHub-style blob hash
/// equals that etag (the official endpoint that publishes SHA256 is unreachable from
/// this host). See `artifacts/p5-semantic/model-provenance.json`.
const MANIFEST: &[FileSpec] = &[
    FileSpec {
        path: "config.json",
        size: 716,
        sha256: "d4193ead3a810fd694fa8a31d7fc72fbaebc0668b603e398734bf2f6538ff42f",
        etag: "d590a6643f5dcdd0cfb0477e3c291488c16fc2d7",
    },
    FileSpec {
        path: "tokenizer.json",
        size: 439_125,
        sha256: "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26",
        etag: "cdb3043fc938fc918c06e66cf704c2ba58f88747",
    },
    FileSpec {
        path: "tokenizer_config.json",
        size: 367,
        sha256: "e6f3b96db926a37d4039995fbf5ad17de158dfb8f6343d607e4dbaad18d75f5a",
        etag: "3a59388f0fd1bd22dec2ce7902c1be8e1fb84107",
    },
    FileSpec {
        path: "special_tokens_map.json",
        size: 125,
        sha256: "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
        etag: "a8b3208c2884c4efb86e49300fdd3dc877220cdf",
    },
    FileSpec {
        path: "onnx/model.onnx",
        size: 94_851_877,
        sha256: "69a0b846f4f116b5e6aabf9546ea6754d02264f3211a13a1bd69b31b8040749a",
        etag: "69a0b846f4f116b5e6aabf9546ea6754d02264f3211a13a1bd69b31b8040749a",
    },
];

/// Download sources, tried in order. `{rev}` and `{path}` are substituted.
const SOURCES: &[(&str, &str)] = &[
    ("hf-mirror", "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/{rev}/{path}"),
    ("huggingface", "https://huggingface.co/Xenova/bge-small-zh-v1.5/resolve/{rev}/{path}"),
];

fn now_rfc3339() -> String {
    // Seconds-resolution timestamp is enough for provenance/`updated_at` bookkeeping.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("@{secs}")
}

/// Model state machine (contract §3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticState {
    Absent,
    Downloading,
    Verifying,
    Ready,
    Failed,
}

impl SemanticState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SemanticState::Absent => "absent",
            SemanticState::Downloading => "downloading",
            SemanticState::Verifying => "verifying",
            SemanticState::Ready => "ready",
            SemanticState::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
struct Status {
    state: SemanticState,
    detail: String,
    /// Only ever set from measured bytes; `None` means "indeterminate progress".
    progress: Option<f64>,
    bytes_downloaded: Option<u64>,
    bytes_total: Option<u64>,
    source: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            state: SemanticState::Absent,
            detail: "模型未下载".into(),
            progress: None,
            bytes_downloaded: None,
            bytes_total: None,
            source: None,
        }
    }
}

/// Per-request semantic knobs (contract §3.1).
#[derive(Clone, Debug)]
pub struct MatchOptions {
    pub semantic: bool,
    pub semantic_min_similarity: f64,
    pub semantic_limit: usize,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            semantic: false,
            semantic_min_similarity: default_min_similarity(),
            semantic_limit: 5,
        }
    }
}

/// Environment-tunable default for `semantic_min_similarity`.
pub fn default_min_similarity() -> f64 {
    std::env::var(ENV_MIN_SIMILARITY)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(DEFAULT_MIN_SIMILARITY)
}

/// A semantic candidate: the memory id plus its cosine similarity.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SemanticHit {
    pub id: String,
    pub score: f64,
}

/// Root directory holding the model's `blobs/`, `refs/` and `snapshots/` folders.
///
/// `GM_EMBED_MODEL_DIR` may be either the `hf-hub` cache root (a directory that already
/// contains `models--Xenova--bge-small-zh-v1.5`) or that repo directory itself.
pub fn model_dir() -> PathBuf {
    if let Ok(custom) = std::env::var(ENV_MODEL_DIR) {
        let custom = custom.trim();
        if !custom.is_empty() {
            let path = PathBuf::from(custom);
            if path.join(MODEL_DIR_NAME).is_dir() {
                return path.join(MODEL_DIR_NAME);
            }
            return path;
        }
    }
    cache_root().join(MODEL_DIR_NAME)
}

/// The `hf-hub` cache root: the directory that contains `models--Org--name`.
fn cache_root() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("USERPROFILE").map(|h| PathBuf::from(h).join("AppData/Local")))
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("GraphMemory").join("models")
}

/// Text that a record contributes to its embedding (contract §2.1).
pub fn record_text(title: &str, content: &str, conditions: &str) -> String {
    normalize_text(&format!("{title}\n{content}\n{conditions}"))
}

/// Whitespace normalization applied before hashing/embedding.
pub fn normalize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' || ch == '\t' || ch == ' ' {
            if !last_space {
                out.push('\n');
                last_space = true;
            }
            continue;
        }
        last_space = false;
        out.push(ch);
    }
    out.trim().to_string()
}

/// SHA256 of the normalized record text; binds `memory_vectors.content_sha256`.
pub fn content_sha256(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// f32 little-endian compact BLOB (contract §4).
pub fn vec_to_blob(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn blob_to_vec(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Cosine similarity of two equal-length vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..a.len() {
        dot += a[i] as f64 * b[i] as f64;
        na += (a[i] as f64) * (a[i] as f64);
        nb += (b[i] as f64) * (b[i] as f64);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Render a status snapshot (contract §3.3): `state` is always present, progress fields
/// only while a download/verification is in flight and only from measured bytes.
pub fn render_status(status: &Status) -> Value {
    let mut value = json!({
        "state": status.state.as_str(),
        "model": MODEL_ID,
        "detail": status.detail,
    });
    if let Some(object) = value.as_object_mut() {
        if status.state == SemanticState::Downloading
            || status.state == SemanticState::Verifying
        {
            object.insert("bytes_total".into(), json!(status.bytes_total));
            object.insert("bytes_downloaded".into(), json!(status.bytes_downloaded));
            object.insert("progress".into(), json!(status.progress));
        }
        if let Some(source) = &status.source {
            object.insert("source".into(), json!(source));
        }
    }
    value
}

/// Runtime shared by the HTTP server and `MemoryStore`.
pub struct SemanticRuntime {
    inner: Mutex<Inner>,
}

struct Inner {
    status: Status,
    embedder: Option<fastembed::TextEmbedding>,
    /// A warmup thread is building the session; prevents duplicate construction.
    loading: bool,
    /// Write-side accumulator used while the model is unavailable.
    dirty: bool,
    /// Vectors computed before the model became ready, keyed by memory id.
    staged: HashMap<String, StagedVector>,
}

struct StagedVector {
    model: String,
    dim: usize,
    content_sha256: String,
    vec: Vec<f32>,
    updated_at: String,
}

impl Default for SemanticRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticRuntime {
    /// Cheap construction: locates the model directory and reads the on-disk state.
    /// Never downloads and never loads ONNX.
    pub fn new() -> Self {
        let runtime = Self {
            inner: Mutex::new(Inner {
                status: Status::default(),
                embedder: None,
                loading: false,
                dirty: false,
                staged: HashMap::new(),
            }),
        };
        runtime.refresh_state();
        runtime
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Inspect the model directory and update `absent`/`ready` accordingly.
    ///
    /// Status only; never hashes (a full SHA256 pass is ~0.5 s and must not run on
    /// startup or on a search). The downloader verifies before installing, and the
    /// loader fails closed if the bytes are not usable.
    fn refresh_state(&self) {
        let dir = model_dir();
        let mut inner = self.lock();
        if inner.status.state == SemanticState::Downloading
            || inner.status.state == SemanticState::Verifying
        {
            return;
        }
        if files_present(&dir).is_ok() {
            if inner.status.state != SemanticState::Ready {
                inner.status = Status {
                    state: SemanticState::Ready,
                    detail: format!("模型已就绪（本地缓存 {}）", dir.display()),
                    progress: None,
                    bytes_downloaded: None,
                    bytes_total: Some(total_bytes()),
                    source: Some("local-cache".into()),
                };
            }
        } else if inner.status.state != SemanticState::Failed {
            inner.status = Status {
                state: SemanticState::Absent,
                detail: format!("模型未下载：{}", dir.display()),
                bytes_total: Some(total_bytes()),
                ..Status::default()
            };
        }
    }

    /// Contract §3.3 payload. `state` is always present; progress fields only while a
    /// download/verification is in flight and only from measured bytes.
    ///
    /// §2.1 v1.5: while a background warm-up is still building the session, `state` stays
    /// `ready` (the files are there) but `detail` says "正在加载" and `loading=true`, so a
    /// client can tell "loading" apart from "absent" before a request degrades.
    pub fn status_json(&self) -> Value {
        let inner = self.lock();
        let mut value = render_status(&inner.status);
        if inner.loading && inner.embedder.is_none() {
            if let Some(object) = value.as_object_mut() {
                object.insert("loading".into(), json!(true));
                object.insert("detail".into(), json!("模型正在加载（后台预热中）"));
            }
        }
        value
    }

    pub fn state(&self) -> SemanticState {
        self.lock().status.state.clone()
    }

    pub fn is_ready(&self) -> bool {
        self.state() == SemanticState::Ready
    }

    /// Is an embedder usable *right now*? Never triggers a load, so this is safe to call
    /// from the request path (a request must never pay for session construction).
    pub fn embedder_ready(&self) -> bool {
        self.lock().embedder.is_some()
    }

    /// Is a background warm-up currently building the session?
    pub fn is_loading(&self) -> bool {
        self.lock().loading
    }

    /// Human-readable reason why the semantic channel could not answer (contract §3.2
    /// `note`). Distinguishes "still loading" from "no model".
    pub fn not_ready_note(&self) -> String {
        let inner = self.lock();
        if inner.loading && inner.embedder.is_none() {
            format!(
                "模型正在加载（后台预热中），本次未在 {} 秒内就绪：语义通道已跳过",
                SESSION_WAIT.as_secs()
            )
        } else {
            format!(
                "模型未就绪（{}）：语义通道已跳过",
                inner.status.state.as_str()
            )
        }
    }

    /// §2.1 v1.5: bounded wait for the background warm-up, so a `semantic=true` request
    /// either answers with suggestions or degrades *once and for all* — never "degraded
    /// now, hybrid three seconds later".
    ///
    /// Returns `true` when the session became usable inside `timeout`; `false` when the
    /// timeout expired, when no load is in flight, or when loading already failed.
    pub fn wait_ready(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.embedder_ready() {
                return true;
            }
            if !self.lock().loading {
                // Nothing is being built: no point waiting (§5: no silent downloads).
                return false;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Build the ONNX session once, on a background thread (called at service startup and
    /// by the explicit `POST /api/v1/models/prepare`). Returns immediately.
    pub fn warmup(self: &Arc<Self>) {
        if self.embedder_ready() {
            return;
        }
        if self.lock().loading {
            return;
        }
        self.lock().loading = true;
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            this.ensure_loaded();
            this.lock().loading = false;
        });
    }

    /// Build the ONNX session from the local directory. Blocking; only called from
    /// [`SemanticRuntime::warmup`] and `prepare`, never from a search.
    pub fn ensure_loaded(&self) -> bool {
        if self.embedder_ready() {
            return true;
        }
        let dir = model_dir();
        if files_present(&dir).is_err() {
            self.refresh_state();
            return false;
        }
        self.load_from_dir(&dir)
    }

    fn embedder_loaded(&self) -> bool {
        self.lock().embedder.is_some()
    }

    fn load_from_dir(&self, dir: &Path) -> bool {
        // Serialize loading: two concurrent searches must not each build an ONNX session.
        {
            let inner = self.lock();
            if inner.embedder.is_some() {
                return true;
            }
        }
        // `fastembed` 6 resolves the cache root from `HF_HOME` and only falls back to
        // `InitOptions::with_cache_dir`; set it for the duration of the load so a
        // `GM_EMBED_MODEL_DIR` override actually takes effect. `hf-hub` expects the cache
        // root (the directory *containing* `models--Org--name`), not the repo directory.
        let cache_root = cache_root_of(dir);
        let previous = std::env::var_os("HF_HOME");
        std::env::set_var("HF_HOME", &cache_root);
        let outcome = fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallZHV15)
                .with_cache_dir(cache_root)
                .with_show_download_progress(false),
        );
        match previous {
            Some(value) => std::env::set_var("HF_HOME", value),
            None => std::env::remove_var("HF_HOME"),
        }
        let mut inner = self.lock();
        if inner.embedder.is_some() {
            return true;
        }
        match outcome {
            Ok(model) => {
                inner.embedder = Some(model);
                inner.status = Status {
                    state: SemanticState::Ready,
                    detail: format!("模型已加载（{} 维，本地离线）", MODEL_DIM),
                    bytes_total: Some(total_bytes()),
                    source: Some("local-cache".into()),
                    ..Status::default()
                };
                true
            }
            Err(error) => {
                let detail = format!("模型加载失败：{error}");
                log::warn!("{detail}");
                inner.embedder = None;
                inner.status = Status {
                    state: SemanticState::Failed,
                    detail,
                    ..Status::default()
                };
                false
            }
        }
    }

    /// Explicit, idempotent model acquisition (contract §3.3 / §5).
    ///
    /// Runs the download on a background thread and returns the status right away so the
    /// HTTP request is not blocked for the whole transfer; `GET /api/v1/agent/status`
    /// then reports measured progress.
    pub fn prepare(self: &Arc<Self>) -> Value {
        let state = self.state();
        if state == SemanticState::Ready {
            // Files are on disk: make sure the session is built, then answer. `warmup`
            // keeps the blocking ONNX construction off this request as well.
            self.warmup();
            return self.status_json();
        }
        if state == SemanticState::Downloading || state == SemanticState::Verifying {
            return self.status_json();
        }
        {
            let mut inner = self.lock();
            inner.status = Status {
                state: SemanticState::Downloading,
                detail: "开始下载模型".into(),
                bytes_downloaded: Some(0),
                bytes_total: Some(total_bytes()),
                progress: Some(0.0),
                source: None,
            };
        }
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let outcome = this.download_and_verify();
            match outcome {
                Ok(source) => {
                    let dir = model_dir();
                    {
                        let mut inner = this.lock();
                        inner.status = Status {
                            state: SemanticState::Ready,
                            detail: format!("模型已就绪（来源 {source}）"),
                            bytes_total: Some(total_bytes()),
                            source: Some(source),
                            ..Status::default()
                        };
                    }
                    this.load_from_dir(&dir);
                }
                Err(error) => {
                    let mut inner = this.lock();
                    inner.status = Status {
                        state: SemanticState::Failed,
                        detail: error,
                        ..Status::default()
                    };
                }
            }
        });
        self.status_json()
    }

    /// Download every manifest file from the first working source, then verify.
    fn download_and_verify(&self) -> Result<String, String> {
        let dir = model_dir();
        if files_present(&dir).is_ok() {
            // Already on disk (possibly from a previous run): verify, then use it.
            self.set_verifying(Some("local-cache".into()), total_bytes(), total_bytes());
            return verify_manifest(&dir)
                .map(|()| "local-cache".to_string())
                .map_err(|e| format!("本地模型校验失败：{e}"));
        }
        let tmp = dir.with_extension("tmp-download");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("无法创建下载目录：{e}"))?;

        let mut failures: Vec<String> = Vec::new();
        for (name, template) in SOURCES {
            match self.download_all(&tmp, template, name) {
                Ok(()) => {
                    self.set_verifying(Some((*name).into()), total_bytes(), total_bytes());
                    match verify_manifest(&tmp) {
                        Ok(()) => {
                            if let Err(error) = install_cache_layout(&tmp, &dir) {
                                let _ = std::fs::remove_dir_all(&tmp);
                                return Err(format!("安装模型缓存失败：{error}"));
                            }
                            let _ = std::fs::remove_dir_all(&tmp);
                            return Ok((*name).to_string());
                        }
                        Err(error) => {
                            failures.push(format!("{name}: 校验失败 {error}"));
                        }
                    }
                }
                Err(error) => failures.push(format!("{name}: {error}")),
            }
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).ok();
        }
        let _ = std::fs::remove_dir_all(&tmp);
        Err(format!("模型下载失败（已尝试所有源）：{}", failures.join("；")))
    }

    fn set_verifying(&self, source: Option<String>, downloaded: u64, total: u64) {
        let mut inner = self.lock();
        inner.status = Status {
            state: SemanticState::Verifying,
            detail: "下载完成，正在逐文件校验大小与 SHA256".into(),
            bytes_downloaded: Some(downloaded),
            bytes_total: Some(total),
            progress: Some(1.0),
            source,
        };
    }

    /// Called by `MemoryStore` when a record changed but no model was loaded yet.
    pub fn mark_index_dirty(&self) {
        self.mark_dirty();
    }

    /// Called by `MemoryStore` before reading the vector index.
    pub fn take_staged(&self) -> (bool, Vec<(String, String, usize, String, Vec<f32>, String)>) {
        let (dirty, staged) = self.take_dirty();
        (
            dirty,
            staged
                .into_iter()
                .map(|(id, v)| {
                    (
                        id,
                        v.model,
                        v.dim,
                        v.content_sha256,
                        v.vec,
                        v.updated_at,
                    )
                })
                .collect(),
        )
    }

    fn set_progress(&self, downloaded: u64, total: u64, source: &str) {
        let mut inner = self.lock();
        inner.status.bytes_downloaded = Some(downloaded);
        inner.status.bytes_total = Some(total);
        inner.status.progress = if total == 0 {
            None
        } else {
            Some((downloaded as f64 / total as f64).clamp(0.0, 1.0))
        };
        inner.status.source = Some(source.to_string());
    }

    /// Download every manifest file into `root` (which mirrors the cache layout),
    /// publishing measured progress after each chunk.
    fn download_all(&self, root: &Path, template: &str, source: &str) -> Result<(), String> {
        let total = total_bytes();
        let mut downloaded = 0u64;
        for spec in MANIFEST {
            let url = template
                .replace("{rev}", MODEL_REVISION)
                .replace("{path}", spec.path);
            let target = snapshot_path(root, spec.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .map_err(|e| e.to_string())?;
            let mut response = client
                .get(&url)
                .send()
                .map_err(|e| format!("{url}: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("{url}: HTTP {}", response.status()));
            }
            if let Some(size) = response.content_length() {
                if size != spec.size {
                    return Err(format!(
                        "{url}: Content-Length={size} 与清单 {} 不符",
                        spec.size
                    ));
                }
            }
            let mut file = std::fs::File::create(&target).map_err(|e| e.to_string())?;
            let mut buffer = vec![0u8; 1 << 20];
            let mut written = 0u64;
            loop {
                let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                std::io::Write::write_all(&mut file, &buffer[..read]).map_err(|e| e.to_string())?;
                written += read as u64;
                downloaded += read as u64;
                self.set_progress(downloaded.min(total), total, source);
            }
            std::io::Write::flush(&mut file).map_err(|e| e.to_string())?;
            if written != spec.size {
                return Err(format!(
                    "{url}: 实际收到 {written} 字节，清单 {}",
                    spec.size
                ));
            }
        }
        Ok(())
    }

    /// Embed one query string. Returns `None` when the model is not ready.
    pub fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        let mut inner = self.lock();
        let model = inner.embedder.as_mut()?;
        match model.embed(vec![text], Some(1)) {
            Ok(mut out) if !out.is_empty() => Some(out.remove(0)),
            Ok(_) => None,
            Err(error) => {
                log::warn!("semantic query embedding failed: {error}");
                None
            }
        }
    }

    /// Embed a batch of record texts.
    pub fn embed_records(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Some(Vec::new());
        }
        let mut inner = self.lock();
        let model = inner.embedder.as_mut()?;
        let refs: Vec<&str> = texts.iter().map(|t| t.as_str()).collect();
        match model.embed(refs, Some(16)) {
            Ok(out) => Some(out),
            Err(error) => {
                log::warn!("semantic record embedding failed: {error}");
                None
            }
        }
    }

    /// Write-through helper: stage or persist one record vector.
    pub fn stage_or_skip(&self, id: &str, text: &str) -> Option<(String, usize, Vec<f32>)> {
        let hash = content_sha256(text);
        {
            let mut inner = self.lock();
            if inner.embedder.is_none() {
                inner.dirty = true;
                inner.staged.remove(id);
                return None;
            }
        }
        let vec = self.embed_query(text)?;
        let dim = vec.len();
        {
            let mut inner = self.lock();
            inner.staged.insert(
                id.to_string(),
                StagedVector {
                    model: MODEL_ID.to_string(),
                    dim,
                    content_sha256: hash.clone(),
                    vec: vec.clone(),
                    updated_at: now_rfc3339(),
                },
            );
            inner.dirty = true;
        }
        Some((hash, dim, vec))
    }

    /// Everything staged since the last flush, plus the dirty flag.
    fn take_dirty(&self) -> (bool, Vec<(String, StagedVector)>) {
        let mut inner = self.lock();
        let dirty = inner.dirty;
        inner.dirty = false;
        let staged = inner.staged.drain().collect();
        (dirty, staged)
    }

    fn mark_dirty(&self) {
        self.lock().dirty = true;
    }
}

fn files_present(dir: &Path) -> Result<(), String> {
    for spec in MANIFEST {
        let path = snapshot_path(dir, spec.path);
        let meta = std::fs::metadata(&path).map_err(|_| format!("缺少 {}", spec.path))?;
        if meta.len() != spec.size {
            return Err(format!(
                "{} 大小不符：磁盘 {} != 清单 {}",
                spec.path,
                meta.len(),
                spec.size
            ));
        }
    }
    Ok(())
}

fn total_bytes() -> u64 {
    MANIFEST.iter().map(|s| s.size).sum()
}

fn snapshot_dir(dir: &Path) -> PathBuf {
    dir.join("snapshots").join(MODEL_REVISION)
}

fn snapshot_path(dir: &Path, relative: &str) -> PathBuf {
    let mut path = snapshot_dir(dir);
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

fn blob_path(dir: &Path, etag: &str) -> PathBuf {
    dir.join("blobs").join(etag)
}

/// `hf-hub` cache root for a repo directory: its parent when the directory is named
/// `models--Org--name` (the cache layout), otherwise the directory itself.
fn cache_root_of(dir: &Path) -> PathBuf {
    if dir.file_name().and_then(|n| n.to_str()) == Some(MODEL_DIR_NAME) {
        dir.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf())
    } else {
        dir.to_path_buf()
    }
}

/// Full manifest verification: size + SHA256 for every file.
fn verify_manifest(dir: &Path) -> Result<(), String> {
    for spec in MANIFEST {
        let path = snapshot_path(dir, spec.path);
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", spec.path))?;
        if meta.len() != spec.size {
            return Err(format!(
                "{} 大小不符：{} != {}",
                spec.path,
                meta.len(),
                spec.size
            ));
        }
        let actual = sha256_file(&path).map_err(|e| format!("{}: {e}", spec.path))?;
        if actual != spec.sha256 {
            return Err(format!(
                "{} SHA256 不符：{} != {}",
                spec.path, actual, spec.sha256
            ));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Move a verified staging tree into the `hf-hub` cache layout (`blobs/`, `refs/`,
/// `snapshots/`) so `fastembed` can load it fully offline.
fn install_cache_layout(source_root: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest.join("blobs")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest.join("refs")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(snapshot_dir(dest)).map_err(|e| e.to_string())?;
    for spec in MANIFEST {
        let from = snapshot_path(source_root, spec.path);
        let blob = blob_path(dest, spec.etag);
        if blob.exists() {
            let _ = std::fs::remove_file(&blob);
        }
        if std::fs::rename(&from, &blob).is_err() {
            std::fs::copy(&from, &blob).map_err(|e| format!("复制 {}: {e}", spec.path))?;
        }
        let to = snapshot_path(dest, spec.path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if to.exists() {
            let _ = std::fs::remove_file(&to);
        }
        if std::fs::hard_link(&blob, &to).is_err() {
            std::fs::copy(&blob, &to).map_err(|e| format!("链接 {}: {e}", spec.path))?;
        }
    }
    std::fs::write(dest.join("refs").join("main"), MODEL_REVISION)
        .map_err(|e| e.to_string())?;
    std::fs::write(dest.join(VERIFIED_MARKER), MODEL_REVISION).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_min_similarity_clears_the_measured_offtopic_ceiling() {
        // Contract v1.4 §3.1/§6.1 evidence C: the 30 probes top out at 0.5160 and the
        // official no_answer set at 0.4647, so the default must be above both.
        assert_eq!(DEFAULT_MIN_SIMILARITY, 0.52);
        assert!(DEFAULT_MIN_SIMILARITY > 0.5160);
        assert_eq!(MatchOptions::default().semantic_min_similarity, 0.52);
        assert_eq!(MatchOptions::default().semantic_limit, 5);
        assert!(!MatchOptions::default().semantic);
    }

    #[test]
    fn normalize_text_collapses_whitespace_runs() {
        assert_eq!(normalize_text("a\n\n  b\tc\r\nd"), "a\nb\nc\nd");
        assert_eq!(normalize_text("  x  "), "x");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn record_text_is_title_content_conditions_joined_by_newline() {
        assert_eq!(record_text("T", "C", "K"), "T\nC\nK");
        assert_eq!(record_text("T", "", ""), "T");
    }

    #[test]
    fn content_sha256_tracks_any_text_change() {
        let a = content_sha256(&record_text("标题", "内容", "条件"));
        let b = content_sha256(&record_text("标题", "内容", "条件2"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(a, content_sha256(&record_text("标题", "内容", "条件")));
    }

    #[test]
    fn blob_roundtrip_preserves_f32_values() {
        let vec = vec![0.0f32, 1.5, -2.25, 1e-9];
        let blob = vec_to_blob(&vec);
        assert_eq!(blob.len(), vec.len() * 4);
        assert_eq!(blob_to_vec(&blob).unwrap(), vec);
        assert!(blob_to_vec(&[0u8; 3]).is_none());
    }

    #[test]
    fn cosine_is_scale_invariant_and_zero_for_mismatch() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-9);
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-9);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn wait_ready_is_bounded_and_reports_loading() {
        // §2.1 v1.5: with no warm-up running the wait must not block, and the note has to
        // say why (absent vs still loading) so a client can tell the two apart.
        let runtime = SemanticRuntime::new();
        let started = std::time::Instant::now();
        assert!(!runtime.wait_ready(SESSION_WAIT));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        assert!(!runtime.is_loading());
        assert!(runtime.not_ready_note().contains("模型未就绪"));
        assert!(!runtime.embedder_ready());
        assert_eq!(SESSION_WAIT, std::time::Duration::from_secs(3));
    }

    #[test]
    fn wait_ready_blocks_while_loading_and_gives_up_at_the_deadline() {
        // §2.1 v1.5: a request must *wait* for an in-flight warm-up (that is what removes
        // the visible flip), but only up to the deadline.
        let runtime = SemanticRuntime::new();
        runtime.lock().loading = true;
        assert!(runtime.not_ready_note().contains("正在加载"));
        let started = std::time::Instant::now();
        assert!(!runtime.wait_ready(std::time::Duration::from_millis(200)));
        let waited = started.elapsed();
        assert!(
            waited >= std::time::Duration::from_millis(200)
                && waited < std::time::Duration::from_millis(1500),
            "bounded wait returned after {waited:?}"
        );
        // Warm-up finished or died: the wait must stop immediately instead of sleeping.
        runtime.lock().loading = false;
        let started = std::time::Instant::now();
        assert!(!runtime.wait_ready(SESSION_WAIT));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn status_hides_progress_unless_transfer_in_flight() {
        let ready = render_status(&Status {
            state: SemanticState::Ready,
            detail: "ok".into(),
            progress: Some(1.0),
            bytes_downloaded: Some(100),
            bytes_total: Some(100),
            source: Some("hf-mirror".into()),
        });
        // Contract §3.3: progress fields must vanish once the transfer is over.
        assert!(ready.get("progress").is_none());
        assert!(ready.get("bytes_downloaded").is_none());
        assert!(ready.get("bytes_total").is_none());
        assert_eq!(ready["state"], "ready");
        assert_eq!(ready["source"], "hf-mirror");

        let downloading = render_status(&Status {
            state: SemanticState::Downloading,
            detail: "…".into(),
            progress: Some(0.25),
            bytes_downloaded: Some(25),
            bytes_total: Some(100),
            source: Some("hf-mirror".into()),
        });
        assert_eq!(downloading["bytes_downloaded"], 25);
        assert_eq!(downloading["bytes_total"], 100);
        assert_eq!(downloading["progress"], 0.25);

        let absent = render_status(&Status::default());
        assert_eq!(absent["state"], "absent");
        assert_eq!(absent["model"], MODEL_ID);
    }

    #[test]
    fn manifest_is_well_formed() {
        let total = total_bytes();
        assert_eq!(total, 95_292_210);
        for spec in MANIFEST {
            assert_eq!(spec.sha256.len(), 64, "{} sha256 length", spec.path);
            assert!(spec.size > 0);
        }
        // The ONNX payload dominates the download; a wrong size here would silently
        // accept a truncated model.
        let onnx = MANIFEST
            .iter()
            .find(|s| s.path == "onnx/model.onnx")
            .expect("onnx entry");
        assert_eq!(onnx.size, 94_851_877);
    }
}

