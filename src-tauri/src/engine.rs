// Graph Memory — Rust 后端核心
// 替代 Python engine.py: 图存储 + embedding + PageRank 检索

use anyhow::{Context, Result};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;

/// 节点数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub content: String,
    pub title: String,
    pub node_type: String,
    pub source: String,
    pub metadata: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 边数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub weight: f64,
    pub metadata: String,
    pub created_at: String,
}

/// 检索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrieveResult {
    pub id: String,
    pub content: String,
    pub title: String,
    pub node_type: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub score: f64,
    pub semantic_score: f64,
    pub pagerank_score: f64,
    pub match_type: String,
}

const DEDUP_THRESHOLD: f32 = 0.85;
const MIN_SIM_THRESHOLD: f32 = 0.3;  // 准入阈值(用于 retrieve,基于原始 cosine)
const AUTO_LINK_THRESHOLD: f32 = 0.5; // B1:auto_link 建边阈值(0.3→0.5,减少噪声边)
const SEED_TOP_K: usize = 2;
const RETRIEVAL_TOP_K: usize = 5;

pub struct GraphEngine {
    db: Connection,
    graph: DiGraph<Node, Edge>,
    node_map: HashMap<String, NodeIndex>,
    embeddings: HashMap<String, Vec<f32>>,
    // embedder 用 RefCell 而非 Option,因为 fastembed::TextEmbedding::embed
    // 需要 &mut self(虽然内部线程安全)。RefCell 在单线程 Mutex 内无额外开销。
    embedder: std::cell::RefCell<Option<fastembed::TextEmbedding>>,
}

impl GraphEngine {
    pub fn new(db_path: &str) -> Result<Self> {
        let db = Connection::open(db_path)?;
        db.execute_batch("PRAGMA journal_mode=WAL;")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (
                id TEXT PRIMARY KEY, content TEXT NOT NULL, title TEXT DEFAULT '',
                node_type TEXT DEFAULT 'knowledge', source TEXT DEFAULT 'manual',
                metadata TEXT DEFAULT '{}', created_at TEXT, updated_at TEXT,
                content_hash TEXT DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS edges (
                source TEXT NOT NULL, target TEXT NOT NULL,
                relation TEXT DEFAULT 'related_to', weight REAL DEFAULT 1.0,
                metadata TEXT DEFAULT '{}', created_at TEXT,
                PRIMARY KEY (source, target)
            );
            CREATE TABLE IF NOT EXISTS node_history (
                node_id TEXT NOT NULL, old_title TEXT, old_content TEXT,
                changed_at TEXT NOT NULL,
                FOREIGN KEY(node_id) REFERENCES nodes(id)
            );
            CREATE INDEX IF NOT EXISTS idx_node_type ON nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_node_source ON nodes(source);
            CREATE INDEX IF NOT EXISTS idx_node_created ON nodes(created_at);
            CREATE TABLE IF NOT EXISTS embeddings (
                node_id TEXT PRIMARY KEY, model TEXT NOT NULL, vector BLOB NOT NULL,
                FOREIGN KEY(node_id) REFERENCES nodes(id)
            );",
        )?;

        // 迁移:nodes 加 access_count / last_accessed 列(阶段3 C1)
        for (col, default) in [("access_count", "0"), ("last_accessed", "")] {
            let has: bool = db.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('nodes') WHERE name='{}'", col),
                [], |r| r.get::<_, i64>(0)
            ).unwrap_or(0) > 0;
            if !has {
                log::info!("Migrating: adding {} column to nodes", col);
                let _ = db.execute(&format!("ALTER TABLE nodes ADD COLUMN {} TEXT DEFAULT '{}'", col, default), []);
            }
        }

        // 迁移:旧表无 content_hash 列时添加(ALTER TABLE 不能用 IF NOT EXISTS)
        let has_content_hash: bool = db.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('nodes') WHERE name='content_hash'",
            [], |r| r.get::<_, i64>(0)
        ).unwrap_or(0) > 0;
        if !has_content_hash {
            log::info!("Migrating: adding content_hash column to nodes");
            let _ = db.execute("ALTER TABLE nodes ADD COLUMN content_hash TEXT DEFAULT ''", []);
        }

        // content_hash 索引(列存在后再建)
        let _ = db.execute("CREATE INDEX IF NOT EXISTS idx_node_hash ON nodes(content_hash)", []);

        // 迁移:旧数据无 content_hash 列时补上(用 content 的 sha256 前 16 位)
        let need_migrate: i64 = db.query_row(
            "SELECT COUNT(*) FROM nodes WHERE content_hash = '' OR content_hash IS NULL",
            [], |r| r.get(0)
        ).unwrap_or(0);
        if need_migrate > 0 {
            log::info!("Migrating {} nodes: backfilling content_hash", need_migrate);
            let mut stmt = db.prepare("SELECT id, content FROM nodes WHERE content_hash = '' OR content_hash IS NULL")?;
            let rows: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .filter_map(|r| r.ok()).collect();
            for (id, content) in rows {
                let hash = Self::content_hash(&content);
                let _ = db.execute("UPDATE nodes SET content_hash = ? WHERE id = ?", params![hash, id]);
            }
            log::info!("Migration done: {} nodes backfilled", need_migrate);
        }

        // FTS5 全文检索(trigram tokenizer,中文友好,不需要 jieba)
        // BM25 关键词搜索:精确匹配型号/端口/路径,弥补 embedding 对标识符的弱点
        let _ = db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                node_id UNINDEXED, title, content, tokenize='trigram'
            );"
        );
        let fts_count: i64 = db.query_row("SELECT COUNT(*) FROM nodes_fts", [], |r| r.get(0)).unwrap_or(0);
        log::info!("FTS5 table: {} entries (trigram tokenizer)", fts_count);

        // 懒加载 embedding 模型(不在启动时加载,避免阻塞窗口创建)
        let mut engine = Self {
            db,
            graph: DiGraph::new(),
            node_map: HashMap::new(),
            embeddings: HashMap::new(),
            embedder: std::cell::RefCell::new(None),
        };
        engine.load_from_db()?;
        Ok(engine)
    }

    fn load_from_db(&mut self) -> Result<()> {
        let mut stmt = self.db.prepare("SELECT id, content, title, node_type, source, metadata, created_at, updated_at FROM nodes")?;
        let nodes = stmt.query_map([], |row| {
            Ok(Node {
                id: row.get(0)?,
                content: row.get(1)?,
                title: row.get(2)?,
                node_type: row.get(3)?,
                source: row.get(4)?,
                metadata: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        for node in nodes {
            let node = node?;
            let idx = self.graph.add_node(node.clone());
            self.node_map.insert(node.id.clone(), idx);
        }

        let mut stmt = self.db.prepare("SELECT source, target, relation, weight, metadata, created_at FROM edges")?;
        let edges = stmt.query_map([], |row| {
            Ok(Edge {
                source: row.get(0)?,
                target: row.get(1)?,
                relation: row.get(2)?,
                weight: row.get(3)?,
                metadata: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        for edge in edges {
            let edge = edge?;
            if let (Some(&s), Some(&t)) = (self.node_map.get(&edge.source), self.node_map.get(&edge.target)) {
                self.graph.add_edge(s, t, edge);
            }
        }
        log::info!("Loaded {} nodes, {} edges", self.graph.node_count(), self.graph.edge_count());

        // G1: 从 SQLite 加载持久化的 embeddings(避免每次重启重新算)
        let emb_count_before = self.embeddings.len();
        if let Ok(mut stmt) = self.db.prepare("SELECT node_id, vector FROM embeddings") {
            if let Ok(rows) = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            }) {
                for row in rows.filter_map(|r| r.ok()) {
                    if let Some(emb) = Self::blob_to_vec(&row.1) {
                        self.embeddings.insert(row.0, emb);
                    }
                }
            }
        }
        let emb_loaded = self.embeddings.len() - emb_count_before;
        if emb_loaded > 0 {
            log::info!("Loaded {} embeddings from SQLite (persisted)", emb_loaded);
        }

        // FTS5 表如果为空但 nodes 有数据(首次建表/迁移),从 nodes 填充
        let fts_count: i64 = self.db.query_row("SELECT COUNT(*) FROM nodes_fts", [], |r| r.get(0)).unwrap_or(0);
        let node_count: i64 = self.db.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap_or(0);
        if fts_count == 0 && node_count > 0 {
            let added = self.db.execute(
                "INSERT INTO nodes_fts(node_id, title, content) SELECT id, title, content FROM nodes",
                [],
            ).unwrap_or(0);
            log::info!("FTS5 populated from existing nodes: {} entries", added);
        }
        Ok(())
    }

    fn content_hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())[..16].to_string()
    }

    /// embedding Vec<f32> → BLOB(小端序)
    fn vec_to_blob(v: &[f32]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(v.len() * 4);
        for &f in v {
            buf.extend_from_slice(&f.to_le_bytes());
        }
        buf
    }

    /// BLOB → embedding Vec<f32>
    fn blob_to_vec(blob: &[u8]) -> Option<Vec<f32>> {
        if blob.len() % 4 != 0 || blob.is_empty() { return None; }
        let mut v = Vec::with_capacity(blob.len() / 4);
        for chunk in blob.chunks_exact(4) {
            let bytes: [u8; 4] = chunk.try_into().ok()?;
            v.push(f32::from_le_bytes(bytes));
        }
        Some(v)
    }

    /// 当前 embedding 模型名(用于持久化/迁移检测)
    fn model_name() -> &'static str {
        "bge-small-zh-v1.5"
    }

    /// 持久化单个 embedding 到 SQLite
    fn persist_embedding(&self, id: &str, vec: &[f32]) {
        let blob = Self::vec_to_blob(vec);
        let _ = self.db.execute(
            "INSERT OR REPLACE INTO embeddings (node_id, model, vector) VALUES (?,?,?)",
            params![id, Self::model_name(), blob],
        );
    }

    /// 生成唯一 node id(UUID v4,不再依赖 content hash)
    fn new_node_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    pub fn set_embedder(&mut self, embedder: fastembed::TextEmbedding) {
        *self.embedder.borrow_mut() = Some(embedder);
    }

    /// embedder 是否已加载(用于判断能否走 embedding 去重/auto_link)
    pub fn embedder_ready(&self) -> bool {
        self.embedder.borrow().is_some()
    }

    // ── FTS5 同步辅助 ──
    /// 插入/更新 FTS5 索引(先删后插,避免重复)
    fn fts_upsert(&self, id: &str, title: &str, content: &str) {
        let _ = self.db.execute("DELETE FROM nodes_fts WHERE node_id = ?", params![id]);
        let _ = self.db.execute(
            "INSERT INTO nodes_fts(node_id, title, content) VALUES(?,?,?)",
            params![id, title, content],
        );
    }
    fn fts_delete(&self, id: &str) {
        let _ = self.db.execute("DELETE FROM nodes_fts WHERE node_id = ?", params![id]);
    }

    /// BM25 关键词搜索(FTS5 trigram)。返回 (node_id, score),score 越高越好。
    /// trigram tokenizer 自动把 query 和 content 切成 3-gram 匹配,中文友好。
    /// 但 FTS5 MATCH 默认 AND 语义——query 里有个词文档没有就全空。
    /// 所以拆成 term(ASCII 串 + CJK 3-gram)用 OR 连接,任何匹配都返回,BM25 排序。
    fn bm25_search(&self, query: &str, limit: usize) -> Vec<(String, f32)> {
        let terms = Self::split_query_terms(query);
        if terms.is_empty() { return Vec::new(); }
        let fts_query = terms.join(" OR ");
        let mut stmt = match self.db.prepare(
            "SELECT node_id, bm25(nodes_fts) as rank FROM nodes_fts
             WHERE nodes_fts MATCH ? ORDER BY rank LIMIT ?"
        ) {
            Ok(s) => s,
            Err(e) => { log::warn!("FTS5 prepare failed: {} (query: {})", e, fts_query); return Vec::new(); }
        };
        let rows = match stmt.query_map(params![fts_query, limit as i64], |row| {
            let id: String = row.get(0)?;
            let rank: f64 = row.get::<_, f64>(1)?;
            Ok((id, -rank as f32))
        }) {
            Ok(r) => r,
            Err(e) => { log::warn!("FTS5 query failed: {} (query: {})", e, fts_query); return Vec::new(); }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// E1: 从 query 提取实体(ASCII 标识符:型号/版本号/端口号/IP)
    /// 用于多实体分路检索。CJK 内容不做实体抽取(太复杂,交给 BM25 trigram)
    fn extract_entities(query: &str) -> Vec<String> {
        let mut entities = Vec::new();
        let mut buf = String::new();
        for ch in query.chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                buf.push(ch);
            } else {
                if buf.len() >= 3 && !entities.contains(&buf) {
                    entities.push(buf.clone());
                }
                buf.clear();
            }
        }
        if buf.len() >= 3 && !entities.contains(&buf) {
            entities.push(buf);
        }
        entities
    }

    /// 把 query 拆成 FTS5 搜索 term:ASCII 字母数字串(如"8397")+ CJK 3-gram(如"板子怎")
    /// 用 OR 连接给 FTS5,避免 AND 语义导致对话词("怎么连")不在文档里就全空
    fn split_query_terms(query: &str) -> Vec<String> {
        let mut terms = Vec::new();
        let mut ascii_buf = String::new();
        let mut cjk_buf: Vec<char> = Vec::new();
        let flush_cjk = |cjk: &mut Vec<char>, terms: &mut Vec<String>| {
            if cjk.len() >= 3 {
                for i in 0..cjk.len() - 2 {
                    terms.push(format!("{}{}{}", cjk[i], cjk[i+1], cjk[i+2]));
                }
            } else if cjk.len() > 0 {
                // 不足 3 字的 CJK,直接用原串(trigram 可能匹配不到,但 OR 不影响其他 term)
                terms.push(cjk.iter().collect());
            }
            cjk.clear();
        };
        for ch in query.chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                if !cjk_buf.is_empty() { flush_cjk(&mut cjk_buf, &mut terms); }
                ascii_buf.push(ch);
            } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
                if !ascii_buf.is_empty() { terms.push(ascii_buf.clone()); ascii_buf.clear(); }
                cjk_buf.push(ch);
            } else {
                if !ascii_buf.is_empty() { terms.push(ascii_buf.clone()); ascii_buf.clear(); }
                if !cjk_buf.is_empty() { flush_cjk(&mut cjk_buf, &mut terms); }
            }
        }
        if !ascii_buf.is_empty() { terms.push(ascii_buf); }
        if !cjk_buf.is_empty() { flush_cjk(&mut cjk_buf, &mut terms); }
        // 去空
        terms.into_iter().filter(|t| !t.is_empty()).collect()
    }

    /// 批量补全:为所有缺 embedding 的节点计算 embedding,并跑 auto_link 建边。
    /// 用于"导入时模型还没就绪 → 先 raw 写入 → 模型就绪后补全"的两阶段流程。
    /// 返回 (补了 embedding 的节点数, 建的边数)。
    pub fn enrich_all(&mut self) -> Result<(usize, usize)> {
        if !self.embedder_ready() {
            return Ok((0, 0));
        }
        // 找缺 embedding 的节点
        let pending: Vec<String> = self.graph.node_indices()
            .map(|i| self.graph[i].id.clone())
            .filter(|id| !self.embeddings.contains_key(id))
            .collect();
        if pending.is_empty() {
            return Ok((0, 0));
        }
        log::info!("enrich_all: {} nodes pending embedding", pending.len());
        for id in &pending {
            if let Some(&idx) = self.node_map.get(id) {
                let content = self.graph[idx].content.clone();
                let emb = self.embed(&content)?;
                self.persist_embedding(id, &emb);
                self.embeddings.insert(id.clone(), emb);
            }
        }
        let mut edges = 0;
        for id in &pending {
            if let Ok(es) = self.auto_link(id, 5) {
                edges += es.len();
            }
        }
        log::info!("enrich_all done: {} embeddings, {} edges", pending.len(), edges);
        Ok((pending.len(), edges))
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut guard = self.embedder.borrow_mut();
        let embedder = guard.as_mut().context("Embedding model not loaded yet")?;
        let embeddings = embedder.embed(vec![text.to_string()], None)?;
        Ok(embeddings.into_iter().next().unwrap_or_default())
    }

    /// 公开 embed(供 llm_extract.rs 跨批次边解析用,不修改图)
    pub fn embed_for_external(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text)
    }

    /// 找全图最接近 query_emb 的节点(cosine >= min_sim)
    /// 供 llm_extract.rs F1 跨批次边解析用
    pub fn find_nearest_embedding(&self, query_emb: &[f32], min_sim: f32) -> Option<(String, f32)> {
        let mut best: Option<(String, f32)> = None;
        for (id, emb) in &self.embeddings {
            let sim = Self::cosine_sim(query_emb, emb);
            if sim >= min_sim {
                if best.is_none() || sim > best.as_ref().unwrap().1 {
                    best = Some((id.clone(), sim));
                }
            }
        }
        best
    }

    fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0. || norm_b == 0. { 0. } else { dot / (norm_a * norm_b) }
    }

    /// B2:边类型在 PPR 扩散中的权重(depends_on 强传播,same_topic 弱传播)
    fn relation_weight(relation: &str) -> f64 {
        match relation {
            "depends_on" => 1.2,
            "part_of" => 1.1,
            "derived_from" => 1.0,
            "strongly_related" => 1.0,
            "related_to" => 0.8,
            "same_topic" => 0.5,
            _ => 0.8,
        }
    }

    /// 直接写入 SQLite(不生成 embedding,用于批量导入)
    pub fn add_node_raw(&mut self, content: &str, title: &str, node_type: &str, source: &str) -> Result<(), String> {
        let nid = Self::new_node_id();
        let chash = Self::content_hash(content);
        let now = Self::now();

        // 第一层:content_hash 精确去重
        if self.db.query_row::<i64, _, _>(
            "SELECT COUNT(*) FROM nodes WHERE content_hash = ?", params![chash],
            |r| r.get(0)
        ).unwrap_or(0) > 0 {
            return Ok(());
        }

        // 第二层:FTS5 模糊去重(防止提炼后重新导入产生重复)
        // 提炼改了 content → content_hash 变 → 精确去重失效
        // 用前 100 字符搜 FTS5,如果 BM25 高分命中 → 已被提炼覆盖,跳过
        let check_text: String = content.chars().take(100).collect();
        let terms = Self::split_query_terms(&check_text);
        if !terms.is_empty() {
            // 用双引号包裹每个 term,避免 FTS5 把大写词当列名(如 "Image")
            let quoted: Vec<String> = terms.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect();
            let fts_query = quoted.join(" OR ");
            if let Ok(mut stmt) = self.db.prepare(
                "SELECT node_id, bm25(nodes_fts) as rank FROM nodes_fts WHERE nodes_fts MATCH ? ORDER BY rank LIMIT 1"
            ) {
                if let Ok(rows) = stmt.query_map(params![fts_query], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                }) {
                    for row in rows.filter_map(|r| r.ok()) {
                        let bm25_score = -row.1;
                        // BM25 > 8.0 表示内容高度相似(已被提炼覆盖或已导入)
                        if bm25_score > 8.0 {
                            return Ok(());
                        }
                    }
                }
            }
        }

        let nt = if is_valid_type(node_type) { node_type.to_string() } else { "knowledge".to_string() };
        let t = if title.is_empty() { content.chars().take(40).collect() } else { title.to_string() };
        let node = Node {
            id: nid.clone(), content: content.to_string(), title: t,
            node_type: nt, source: source.to_string(), metadata: "{}".to_string(),
            created_at: now.clone(), updated_at: now,
        };

        self.db.execute(
            "INSERT OR REPLACE INTO nodes (id, content, title, node_type, source, metadata, created_at, updated_at, content_hash) VALUES (?,?,?,?,?,?,?,?,?)",
            params![node.id, node.content, node.title, node.node_type, node.source, node.metadata, node.created_at, node.updated_at, &chash],
        ).map_err(|e| e.to_string())?;
        self.fts_upsert(&node.id, &node.title, &node.content);

        let idx = self.graph.add_node(node);
        self.node_map.insert(nid, idx);
        Ok(())
    }

    pub fn add_node(&mut self, content: &str, title: &str, node_type: &str, source: &str, metadata: &str) -> Result<Node> {
        let now = Self::now();
        let chash = Self::content_hash(content);

        // 第一层: content_hash 精确去重(不再依赖 id=hash)
        let dup_id: Option<String> = self.db.query_row(
            "SELECT id FROM nodes WHERE content_hash = ?", params![chash],
            |r| r.get(0)
        ).ok();
        if let Some(existing_id) = dup_id {
            if let Some(&idx) = self.node_map.get(&existing_id) {
                self.graph[idx].updated_at = now.clone();
                let _ = self.db.execute("UPDATE nodes SET updated_at = ? WHERE id = ?", params![&now, existing_id]);
                return Ok(self.graph[idx].clone());
            }
        }

        // 模型未就绪时:跳过 embedding 相似度去重,走 raw 写入(hash 去重已覆盖第一层)
        if !self.embedder_ready() {
            return self.add_node_raw_fallback(content, title, node_type, source, metadata);
        }

        // 第二层: embedding 相似度去重(>=0.85)
        let new_emb = self.embed(content)?;
        for (existing_id, existing_emb) in &self.embeddings {
            let sim = Self::cosine_sim(&new_emb, existing_emb);
            if sim >= DEDUP_THRESHOLD {
                if let Some(&idx) = self.node_map.get(existing_id) {
                    self.graph[idx].updated_at = now.clone();
                    let _ = self.db.execute("UPDATE nodes SET updated_at = ? WHERE id = ?", params![&now, existing_id]);
                    // D1 修复:不再静默丢弃新内容。调用方(写 API)能通过返回的已存在节点
                    // 判断这是去重命中,而非新建。新内容在写 API 层走 update 路径。
                    log::info!("add_node: near-duplicate (sim={:.3}) of {}, new content preserved via API layer", sim, existing_id);
                    return Ok(self.graph[idx].clone());
                }
            }
        }

        // 第三层: 新建(UUID id,不再依赖 content hash)
        let nid = Self::new_node_id();
        let nt = if is_valid_type(node_type) { node_type.to_string() } else { "knowledge".to_string() };
        let t = if title.is_empty() { content.chars().take(40).collect() } else { title.to_string() };
        let node = Node {
            id: nid.clone(),
            content: content.to_string(),
            title: t,
            node_type: nt,
            source: source.to_string(),
            metadata: if metadata.is_empty() { "{}".to_string() } else { metadata.to_string() },
            created_at: now.clone(),
            updated_at: now,
        };

        self.db.execute(
            "INSERT OR REPLACE INTO nodes (id, content, title, node_type, source, metadata, created_at, updated_at, content_hash) VALUES (?,?,?,?,?,?,?,?,?)",
            params![node.id, node.content, node.title, node.node_type, node.source, node.metadata, node.created_at, node.updated_at, &chash],
        )?;
        self.fts_upsert(&node.id, &node.title, &node.content);

        let idx = self.graph.add_node(node.clone());
        self.node_map.insert(nid.clone(), idx);
        self.persist_embedding(&nid, &new_emb);
        self.embeddings.insert(nid, new_emb);
        Ok(node)
    }

    /// add_node 的降级路径:embedder 未就绪时,跳过 embedding 相似度去重,
    /// 直接 raw 写入(content_hash 去重已在第一层覆盖)。模型就绪后由 enrich_all 补 embedding。
    fn add_node_raw_fallback(&mut self, content: &str, title: &str, node_type: &str, source: &str, metadata: &str) -> Result<Node> {
        let nt = if is_valid_type(node_type) { node_type.to_string() } else { "knowledge".to_string() };
        let t = if title.is_empty() { content.chars().take(40).collect() } else { title.to_string() };
        let now = Self::now();
        let nid = Self::new_node_id();
        let chash = Self::content_hash(content);
        let node = Node {
            id: nid.clone(),
            content: content.to_string(),
            title: t,
            node_type: nt,
            source: source.to_string(),
            metadata: if metadata.is_empty() { "{}".to_string() } else { metadata.to_string() },
            created_at: now.clone(),
            updated_at: now,
        };
        self.db.execute(
            "INSERT OR REPLACE INTO nodes (id, content, title, node_type, source, metadata, created_at, updated_at, content_hash) VALUES (?,?,?,?,?,?,?,?,?)",
            params![node.id, node.content, node.title, node.node_type, node.source, node.metadata, node.created_at, node.updated_at, &chash],
        )?;
        self.fts_upsert(&nid, &node.title, &node.content);
        let idx = self.graph.add_node(node.clone());
        self.node_map.insert(node.id.clone(), idx);
        Ok(node)
    }

    pub fn auto_link(&mut self, nid: &str, max_links: usize) -> Result<Vec<Edge>> {
        let new_emb = self.embeddings.get(nid).cloned()
            .context("Node embedding not found")?;

        let mut sims: Vec<(String, f32)> = self.embeddings.iter()
            .filter(|(id, _)| *id != nid)
            .map(|(id, emb)| (id.clone(), Self::cosine_sim(&new_emb, emb)))
            .collect();
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        let mut created = Vec::new();
        for (target_id, sim) in sims.into_iter().take(max_links) {
            // B1:阈值 0.3→0.5,减少噪声边(BGE-Small 0.3 太松,9边/节点是毛球)
            if sim < AUTO_LINK_THRESHOLD { break; }
            // 已存在同向边则跳过(避免重复)
            if let (Some(&s_idx), Some(&t_idx)) = (self.node_map.get(nid), self.node_map.get(&target_id)) {
                if self.graph.find_edge(s_idx, t_idx).is_some() { continue; }
            }
            let relation = if sim > 0.7 { "strongly_related" } else { "related_to" };
            let edge = Edge {
                source: nid.to_string(),
                target: target_id.clone(),
                relation: relation.to_string(),
                weight: sim as f64,
                metadata: r#"{"auto_linked":true}"#.to_string(),
                created_at: Self::now(),
            };
            self.db.execute(
                "INSERT OR REPLACE INTO edges VALUES (?,?,?,?,?,?)",
                params![edge.source, edge.target, edge.relation, edge.weight, edge.metadata, edge.created_at],
            )?;
            if let (Some(&s), Some(&t)) = (self.node_map.get(&edge.source), self.node_map.get(&edge.target)) {
                self.graph.add_edge(s, t, edge.clone());
            }
            created.push(edge);
        }
        Ok(created)
    }

    /// 添加一条显式边(LLM 标注的关系)。返回 ()。失败(节点不存在)返回 Err。
    pub fn add_edge(&mut self, source: &str, target: &str, relation: &str, weight: f64, metadata: &str) -> Result<()> {
        if !self.node_map.contains_key(source) || !self.node_map.contains_key(target) {
            anyhow::bail!("node not found: {} / {}", source, target);
        }
        let edge = Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation: relation.to_string(),
            weight,
            metadata: metadata.to_string(),
            created_at: Self::now(),
        };
        self.db.execute(
            "INSERT OR REPLACE INTO edges VALUES (?,?,?,?,?,?)",
            params![edge.source, edge.target, edge.relation, edge.weight, edge.metadata, edge.created_at],
        )?;
        if let (Some(&s), Some(&t)) = (self.node_map.get(&edge.source), self.node_map.get(&edge.target)) {
            self.graph.add_edge(s, t, edge);
        }
        Ok(())
    }

    pub fn retrieve(&mut self, query: &str, top_k: Option<usize>, spread: bool) -> Result<Vec<RetrieveResult>> {
        let top_k = top_k.unwrap_or(RETRIEVAL_TOP_K);

        // ── 混合检索:BM25(关键词) + Embedding(语义) → RRF 融合 ──
        // BM25:精确匹配型号/端口/路径(FTS5 trigram,中文友好)
        // Embedding:概念匹配,同义词/意图
        // RRF:按排名融合,不依赖分数尺度,工业标准

        // E1: 实体抽取 + 分路检索
        // 从 query 提取 ASCII 标识符(型号/端口/路径),对每个实体单独 BM25 查,
        // 合并种子集。多实体查询("8397板子和A800的区别")能覆盖两个实体邻域。
        let entities = Self::extract_entities(query);
        let mut bm25_results = self.bm25_search(query, top_k * 4);
        // 每个实体单独 BM25(补充主查询可能漏掉的实体精确匹配)
        for ent in &entities {
            if ent.len() >= 3 {  // 太短的实体不单独查(如"A8")
                let ent_results = self.bm25_search(ent, top_k * 2);
                bm25_results.extend(ent_results);
            }
        }
        // 去重(同 id 取更高分)
        let mut seen_bm: HashMap<String, f32> = HashMap::new();
        for (id, score) in &bm25_results {
            seen_bm.entry(id.clone()).and_modify(|s| *s = s.max(*score)).or_insert(*score);
        }
        let mut bm25_results: Vec<(String, f32)> = seen_bm.into_iter().collect();
        bm25_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // 模型没就绪:只用 BM25(不需要 embedding)
        if !self.embedder_ready() || self.embeddings.is_empty() {
            return Ok(bm25_results.iter().take(top_k)
                .filter_map(|(id, score)| {
                    self.node_map.get(id).map(|&idx| {
                        let node = &self.graph[idx];
                        RetrieveResult {
                            id: node.id.clone(), content: node.content.clone(),
                            title: node.title.clone(), node_type: node.node_type.clone(),
                            source: node.source.clone(), created_at: node.created_at.clone(),
                            updated_at: node.updated_at.clone(),
                            score: *score as f64, semantic_score: 0.0,
                            pagerank_score: 0.0, match_type: "keyword".to_string(),
                        }
                    })
                })
                .collect());
        }

        let query_emb = self.embed(query)?;

        // 2. Embedding 语义搜索(O(N) 遍历,N 小时够快)
        let mut sem_sims: Vec<(String, f32)> = self.embeddings.iter()
            .map(|(id, emb)| (id.clone(), Self::cosine_sim(&query_emb, emb)))
            .collect();
        sem_sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // 3. Reciprocal Rank Fusion(k=60,标准值)
        let rrf_k = 60.0;
        let mut rrf_scores: HashMap<String, f32> = HashMap::new();
        for (rank, (id, _)) in bm25_results.iter().enumerate() {
            *rrf_scores.entry(id.clone()).or_insert(0.0) += 1.0 / (rrf_k + rank as f32 + 1.0);
        }
        for (rank, (id, _)) in sem_sims.iter().take(top_k * 4).enumerate() {
            *rrf_scores.entry(id.clone()).or_insert(0.0) += 1.0 / (rrf_k + rank as f32 + 1.0);
        }
        // 归一化 RRF 到 [0,1](用于排序)
        let max_rrf = rrf_scores.values().cloned().fold(0.0f32, f32::max).max(1e-8);
        let mut sims: Vec<(String, f32)> = rrf_scores.iter()
            .map(|(id, score)| (id.clone(), score / max_rrf))
            .collect();
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // A1 修复:构建 sem_map 做准入门槛(O(1) 查,避免重复算 cosine)
        // 准入条件:原始 cosine >= 0.2 OR BM25 命中(任一即可)
        // RRF 只做排序,不做门槛(RRF 归一化后所有候选 >0.3,门槛是死代码)
        let sem_map: HashMap<String, f32> = sem_sims.iter().cloned().collect();
        let bm25_set: std::collections::HashSet<String> = bm25_results.iter().map(|(id, _)| id.clone()).collect();
        let is_relevant = |id: &str| -> bool {
            let sem = sem_map.get(id).copied().unwrap_or(0.0);
            sem >= 0.2 || bm25_set.contains(id)
        };

        let seeds: Vec<(String, f32)> = sims.iter().take(SEED_TOP_K)
            .filter(|(id, _)| is_relevant(id))
            .cloned()
            .collect();
        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        if !spread {
            return Ok(sims.iter().take(top_k)
                .filter(|(id, _)| is_relevant(id))
                .filter_map(|(id, sim)| {
                    self.node_map.get(id).map(|&idx| {
                        let node = &self.graph[idx];
                        let sem = sem_map.get(id).copied().unwrap_or(0.0);
                        let is_kw = bm25_set.contains(id);
                        let mt = if is_kw && sem >= 0.2 { "hybrid" } else if is_kw { "keyword" } else { "semantic" };
                        RetrieveResult {
                            id: node.id.clone(), content: node.content.clone(),
                            title: node.title.clone(), node_type: node.node_type.clone(),
                            source: node.source.clone(), created_at: node.created_at.clone(),
                            updated_at: node.updated_at.clone(),
                            score: *sim as f64, semantic_score: sem as f64,
                            pagerank_score: 0.0, match_type: mt.to_string(),
                        }
                    })
                })
                .collect());
        }

        // 4. 真 Personalized PageRank(PPR):多跳传播
        // 替代 1-hop boost。PPR 迭代:p = α·seed + (1-α)·W^T·p
        // α=0.5(50% 留在种子,50% 沿边传播),3 轮=3 跳
        // B2:边类型参与扩散(depends_on 权重高,same_topic 低)
        // B3:双向扩散(edges_directed Both,不只 out-edges)
        use petgraph::Direction;
        let damping = 0.5;
        let iterations = 3;

        // 种子分:非种子=0,种子=其 RRF 归一化分
        let seed_map: HashMap<String, f32> = seeds.iter().cloned().collect();

        // A2 修复:PPR 初始化必须覆盖所有候选(BM25-only 节点不在 sem_sims 里)
        // 否则关键词命中的节点 PPR=0,永远拿不到传播分
        let mut ppr: HashMap<String, f64> = HashMap::new();
        // 语义候选
        for (id, _) in &sem_sims {
            ppr.insert(id.clone(), seed_map.get(id).copied().unwrap_or(0.0) as f64);
        }
        // BM25-only 候选(不在 sem_sims 里的 BM25 命中节点)
        for (id, _) in &bm25_results {
            ppr.entry(id.clone()).or_insert(seed_map.get(id).copied().unwrap_or(0.0) as f64);
        }

        // PPR 迭代
        for _iter in 0..iterations {
            let mut new_ppr: HashMap<String, f64> = HashMap::new();
            // A2 修复:遍历所有 ppr 节点(不只是 sem_sims)
            for (id, _) in &ppr {
                let idx = match self.node_map.get(id) { Some(&i) => i, None => continue };
                // 从邻居收集分数(双向 B3:Outgoing + Incoming)
                let mut incoming: f64 = 0.0;
                let mut degree: usize = 0;
                // Outgoing 边:邻居 = target
                for edge_ref in self.graph.edges_directed(idx, Direction::Outgoing) {
                    let neighbor_id = &self.graph[edge_ref.target()].id;
                    let neighbor_p = ppr.get(neighbor_id).copied().unwrap_or(0.0);
                    let rw = Self::relation_weight(&edge_ref.weight().relation);
                    incoming += neighbor_p * edge_ref.weight().weight * rw;
                    degree += 1;
                }
                // Incoming 边:邻居 = source
                for edge_ref in self.graph.edges_directed(idx, Direction::Incoming) {
                    let neighbor_id = &self.graph[edge_ref.source()].id;
                    let neighbor_p = ppr.get(neighbor_id).copied().unwrap_or(0.0);
                    let rw = Self::relation_weight(&edge_ref.weight().relation);
                    incoming += neighbor_p * edge_ref.weight().weight * rw;
                    degree += 1;
                }
                let teleport = seed_map.get(id).copied().unwrap_or(0.0) as f64;
                let propagated = incoming / degree.max(1) as f64;
                new_ppr.insert(id.clone(), damping * teleport + (1.0 - damping) * propagated);
            }
            // A2 修复:保留上一轮有分数但本轮无邻居的节点(不被丢弃)
            for (id, score) in &ppr {
                new_ppr.entry(id.clone()).or_insert(damping * score);
            }
            ppr = new_ppr;
        }

        // A3 修复:归一化 PPR 到 [0,1](和 RRF 同尺度,融合权重要有意义)
        let max_ppr = ppr.values().cloned().fold(0.0f64, f64::max).max(1e-8);

        let type_priority: HashMap<&str, f64> = [
            ("knowledge", 0.0), ("project", 1.0), ("fact", 2.0),
            ("reference", 3.0), ("preference", 4.0), ("skill", 5.0),
        ].iter().cloned().collect();

        let mut results: Vec<(RetrieveResult, f64)> = self.embeddings.keys()
            .filter_map(|id| {
                // A1 修复:准入用原始 cosine + BM25 命中,不用 RRF 归一化分
                if !is_relevant(id) { return None; }
                let rrf = sims.iter().find(|(sid, _)| sid == id).map(|(_, s)| *s).unwrap_or(0.0);
                let sem_orig = sem_map.get(id).copied().unwrap_or(0.0);
                let pr = ppr.get(id).copied().unwrap_or(0.0) / max_ppr;
                let fused = 0.5 * rrf as f64 + 0.5 * pr;
                let idx = self.node_map.get(id)?;
                let node = &self.graph[*idx];
                let priority = type_priority.get(node.node_type.as_str()).copied().unwrap_or(9.0);
                let sort_key = (fused * 100.0).round() / 100.0 - 0.01 * priority;
                let is_kw = bm25_set.contains(id);
                let mt = if is_kw && sem_orig >= 0.2 { "hybrid_spread" } else if is_kw { "keyword_spread" } else { "graph_spread" };
                Some((RetrieveResult {
                    id: node.id.clone(), content: node.content.clone(),
                    title: node.title.clone(), node_type: node.node_type.clone(),
                    source: node.source.clone(), created_at: node.created_at.clone(),
                    updated_at: node.updated_at.clone(),
                    score: fused, semantic_score: sem_orig as f64, pagerank_score: pr,
                    match_type: mt.to_string(),
                }, sort_key))
            })
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // C1:时间衰减 + 访问频次加成
        // 常用的节点(被检索命中过)和近期更新的节点获得加成
        let now_ts = chrono::Utc::now();
        for (r, sort_key) in results.iter_mut() {
            let idx = match self.node_map.get(&r.id) { Some(&i) => i, None => continue };
            let node = &self.graph[idx];
            // 访问计数:从 SQLite 读(节点可能被多次检索命中)
            let access_count: i64 = self.db.query_row(
                "SELECT COALESCE(CAST(access_count AS INTEGER), 0) FROM nodes WHERE id = ?",
                params![&r.id], |row| row.get(0)
            ).unwrap_or(0);
            let access_boost = (1.0 + access_count as f64).ln() * 0.05;  // log(1+n)*0.05, 最多 ~0.15

            // 时间衰减:90 天半衰期,exp(-Δt/(90*86400))
            let updated = chrono::DateTime::parse_from_rfc3339(&node.updated_at).ok();
            let decay = if let Some(t) = updated {
                let age_days = (now_ts - t.with_timezone(&chrono::Utc)).num_days().max(0) as f64;
                (-age_days / 90.0).exp()  // 90 天衰减到 ~37%
            } else { 1.0 };

            *sort_key = *sort_key * (1.0 + access_boost) * (0.5 + 0.5 * decay);  // 衰减最多减半,不完全归零
        }
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // H2:MMR 多样性——去重近似结果,避免 top_k 被同主题碎片占满
        // 贪心选:每次选分数最高的,但惩罚与已选结果 embedding 太近的(>0.85)
        // A3 修复:无 embedding 的节点不参与 MMR 判定(和谁都不"太相似")
        let mut selected: Vec<(RetrieveResult, f64)> = Vec::new();
        let mut remaining = results;
        while selected.len() < top_k && !remaining.is_empty() {
            remaining.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let (best, best_key) = remaining.remove(0);
            // 只在两边都有 embedding 时才判 MMR
            let best_emb = match self.embeddings.get(&best.id) {
                Some(e) => e,
                None => { selected.push((best, best_key)); continue; }  // 无 embedding,直接选
            };
            let mut too_similar = false;
            for s in &selected {
                if let Some(s_emb) = self.embeddings.get(&s.0.id) {
                    let sim = Self::cosine_sim(best_emb, s_emb);
                    if sim >= 0.85 { too_similar = true; break; }
                }
            }
            if !too_similar {
                selected.push((best, best_key));
            }
            // 如果太相似,跳过(不加入 selected,也不放回 remaining)
        }

        // C1:更新命中节点的 access_count + last_accessed
        // A1 修复:合并为单条 SQL(WAL 模式下比逐条写快,减少锁持有时间)
        let now_str = now_ts.to_rfc3339();
        let hit_ids: Vec<&str> = selected.iter().map(|(r, _)| r.id.as_str()).collect();
        if !hit_ids.is_empty() {
            let placeholders = hit_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE nodes SET access_count = COALESCE(CAST(access_count AS INTEGER), 0) + 1, last_accessed = ? WHERE id IN ({})",
                placeholders
            );
            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(now_str)];
            for id in &hit_ids { params_vec.push(Box::new(id.to_string())); }
            let params_ref: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
            let _ = self.db.execute(&sql, params_ref.as_slice());
        }

        Ok(selected.into_iter().map(|(r, _)| r).collect())
    }

    pub fn stats(&self) -> serde_json::Value {
        // 类型/来源分布(遍历全图,供前端筛选器显示准确计数)
        let mut type_counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut source_counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for node in self.graph.node_weights() {
            *type_counts.entry(node.node_type.clone()).or_insert(0) += 1;
            *source_counts.entry(node.source.clone()).or_insert(0) += 1;
        }
        let edge_count = self.graph.edge_count();
        let max_edges = self.graph.node_count() * (self.graph.node_count().saturating_sub(1));
        let density = if max_edges > 0 { edge_count as f64 / max_edges as f64 } else { 0.0 };
        serde_json::json!({
            "node_count": self.graph.node_count(),
            "edge_count": edge_count,
            "density": (density * 1000.0).round() / 1000.0,
            "type_counts": type_counts,
            "source_counts": source_counts,
        })
    }

    /// 返回所有节点的 (id, title, content, source),供 LLM 重提炼用
    pub fn all_nodes_raw(&self) -> Vec<(String, String, String, String)> {
        self.graph
            .node_indices()
            .map(|idx| {
                let n = &self.graph[idx];
                (n.id.clone(), n.title.clone(), n.content.clone(), n.source.clone())
            })
            .collect()
    }

    // ── I1: 维护工具(consolidate / forget) ────────────────

    /// 合并两个节点:保留 A(更丰富的),把 B 的边迁移到 A,B 删除。
    /// B 的 content 追加到 A 的 metadata 里(不丢信息)。
    /// 返回合并后的节点 A。
    pub fn consolidate(&mut self, keep_id: &str, merge_id: &str) -> Result<Node> {
        if keep_id == merge_id {
            anyhow::bail!("cannot consolidate a node with itself");
        }
        let merge_idx = *self.node_map.get(merge_id)
            .with_context(|| format!("merge node not found: {}", merge_id))?;
        let keep_idx = *self.node_map.get(keep_id)
            .with_context(|| format!("keep node not found: {}", keep_id))?;

        // 1. 存历史(B 的内容)
        let merge_node = self.graph[merge_idx].clone();
        self.db.execute(
            "INSERT INTO node_history (node_id, old_title, old_content, changed_at) VALUES (?,?,?,?)",
            params![merge_id, &merge_node.title, &merge_node.content, Self::now()],
        )?;

        // 2. 把 B 的所有边迁移到 A(source/target 中 merge_id 替换为 keep_id)
        let edges_to_migrate: Vec<(String, String, String, f64, String, String)> = {
            let mut stmt = self.db.prepare(
                "SELECT source, target, relation, weight, metadata, created_at FROM edges WHERE source = ? OR target = ?"
            )?;
            let rows = stmt.query_map(params![merge_id, merge_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for (src, tgt, rel, w, meta, cat) in edges_to_migrate {
            let new_src = if src == merge_id { keep_id.to_string() } else { src };
            let new_tgt = if tgt == merge_id { keep_id.to_string() } else { tgt };
            if new_src != new_tgt {  // 不自环
                self.db.execute(
                    "INSERT OR REPLACE INTO edges VALUES (?,?,?,?,?,?)",
                    params![new_src, new_tgt, rel, w, meta, cat],
                )?;
                if let (Some(&s), Some(&t)) = (self.node_map.get(&new_src), self.node_map.get(&new_tgt)) {
                    self.graph.add_edge(s, t, Edge {
                        source: new_src, target: new_tgt, relation: rel, weight: w, metadata: meta, created_at: cat,
                    });
                }
            }
        }

        // 3. 删 B 的旧边(已在 delete_node 时清理,但这里先清 SQLite)
        self.db.execute("DELETE FROM edges WHERE source = ? OR target = ?", params![merge_id, merge_id])?;

        // 4. 在 A 的 metadata 记录合并来源(先提取 metadata 字符串,避免借用冲突)
        let old_meta = self.graph[keep_idx].metadata.clone();
        let merge_info = format!(r#"{{"merged_from": "{}", "merged_title": "{}"}}"#, merge_id, merge_node.title);
        let new_meta = if old_meta == "{}" { merge_info } else { format!(r#"{},"merged_from":"{}""#, old_meta, merge_id) };

        // 5. 删 B(节点 + embedding + FTS + history 保留)
        self.delete_node(merge_id);

        // 6. 更新 A 的 metadata(在 delete_node 之后,避免借用冲突)
        if let Some(&idx) = self.node_map.get(keep_id) {
            self.graph[idx].metadata = new_meta.clone();
        }
        self.db.execute(
            "UPDATE nodes SET metadata = ? WHERE id = ?",
            params![&new_meta, keep_id],
        )?;

        log::info!("Consolidated {} into {}", merge_id, keep_id);
        Ok(self.graph[keep_idx].clone())
    }

    /// 遗忘:删除超过 N 天未访问且 access_count 低于阈值的节点。
    /// 返回 (删除数, 保留数)。
    pub fn forget_stale(&mut self, max_age_days: i64, min_access_count: i64) -> (usize, usize) {
        let now = chrono::Utc::now();
        let stale: Vec<String> = self.graph.node_indices().filter_map(|idx| {
            let n = &self.graph[idx];
            // 解析 last_accessed(空则用 updated_at)
            let la_str = {
                let la = self.db.query_row(
                    "SELECT COALESCE(last_accessed, updated_at) FROM nodes WHERE id = ?",
                    params![&n.id], |r| r.get::<_, String>(0)
                ).unwrap_or_default();
                la
            };
            let access_count: i64 = self.db.query_row(
                "SELECT COALESCE(CAST(access_count AS INTEGER), 0) FROM nodes WHERE id = ?",
                params![&n.id], |r| r.get(0)
            ).unwrap_or(0);

            let age_days = chrono::DateTime::parse_from_rfc3339(&la_str).ok()
                .map(|t| (now - t.with_timezone(&chrono::Utc)).num_days())
                .unwrap_or(0);

            if age_days > max_age_days && access_count < min_access_count {
                // 检查度:有 >=2 条边的节点保留(是图中桥节点)
                let degree = self.graph.edges_directed(idx, petgraph::Direction::Outgoing).count()
                    + self.graph.edges_directed(idx, petgraph::Direction::Incoming).count();
                if degree >= 2 { return None; }
                Some(n.id.clone())
            } else { None }
        }).collect();

        let deleted = stale.len();
        let kept = self.graph.node_count() - deleted;
        for id in &stale {
            self.delete_node(id);
        }
        log::info!("forget_stale: deleted {} nodes (>{} days, <{} accesses, degree<2)", deleted, max_age_days, min_access_count);
        (deleted, kept)
    }

    /// 去重扫描(只读,零 LLM token):找 embedding 0.7-0.85 的近似对,
    /// 返回候选列表供前端展示 / agent 用 consolidate 合并。不自动合并。
    pub fn dedup_scan(&self, min_sim: f32, max_sim: f32, limit: usize) -> Vec<serde_json::Value> {
        let ids: Vec<String> = self.embeddings.keys().cloned().collect();
        let mut pairs = Vec::new();
        for i in 0..ids.len() {
            for j in (i+1)..ids.len() {
                let a = &self.embeddings[&ids[i]];
                let b = &self.embeddings[&ids[j]];
                let sim = Self::cosine_sim(a, b);
                if sim >= min_sim && sim < max_sim {
                    let ai = self.node_map.get(&ids[i]).copied();
                    let bi = self.node_map.get(&ids[j]).copied();
                    if let (Some(ai), Some(bi)) = (ai, bi) {
                        let na = &self.graph[ai];
                        let nb = &self.graph[bi];
                        pairs.push(serde_json::json!({
                            "id_a": na.id, "id_b": nb.id,
                            "title_a": na.title, "title_b": nb.title,
                            "similarity": sim,
                        }));
                    }
                }
                if pairs.len() >= limit { return pairs; }
            }
        }
        pairs
    }

    /// 检查节点是否已被提炼过(metadata 含 "refined":true)
    pub fn is_refined(&self, id: &str) -> bool {
        if let Some(&idx) = self.node_map.get(id) {
            self.graph[idx].metadata.contains("\"refined\":true")
        } else { false }
    }

    /// 标记节点已提炼(metadata 加 "refined":true)
    pub fn mark_refined(&mut self, id: &str) {
        if let Some(&idx) = self.node_map.get(id) {
            let node = &mut self.graph[idx];
            if !node.metadata.contains("\"refined\":true") {
                let old = node.metadata.trim_end_matches('}').trim_end_matches(',');
                let new = if old == "{" || old.is_empty() {
                    r#"{"refined":true}"#.to_string()
                } else {
                    format!(r#"{},"refined":true}}"#, old)
                };
                node.metadata = new;
                let _ = self.db.execute(
                    "UPDATE nodes SET metadata = ? WHERE id = ?",
                    params![&node.metadata, id],
                );
            }
        }
    }

    /// 检查节点是否存在
    pub fn node_exists(&self, id: &str) -> bool {
        self.node_map.contains_key(id)
    }

    /// 获取节点标题
    pub fn get_node_title(&self, id: &str) -> Option<String> {
        self.node_map.get(id).map(|&idx| self.graph[idx].title.clone())
    }

    /// 更新节点的标题和内容(保留 id, type, source, metadata)
    /// C2 修复:存历史到 node_history 表,可追溯/恢复
    /// P1 修复:同步更新 embedding(内容变后旧 embedding 不再匹配)
    pub fn update_node_text(&mut self, id: &str, new_title: &str, new_content: &str) -> Result<()> {
        let idx = *self
            .node_map
            .get(id)
            .context("node not found")?;
        let node = &mut self.graph[idx];
        let now = chrono::Utc::now().to_rfc3339();

        // 存历史(旧标题+旧内容)到 node_history,可追溯
        self.db.execute(
            "INSERT INTO node_history (node_id, old_title, old_content, changed_at) VALUES (?,?,?,?)",
            params![id, &node.title, &node.content, &now],
        )?;

        node.title = new_title.to_string();
        node.content = new_content.to_string();
        node.updated_at = now.clone();
        // 同步到 SQLite + FTS5(content_hash 也更新,保持一致性)
        let chash = Self::content_hash(new_content);
        self.db.execute(
            "UPDATE nodes SET title = ?, content = ?, updated_at = ?, content_hash = ? WHERE id = ?",
            params![new_title, new_content, &now, &chash, id],
        )?;
        self.fts_upsert(id, new_title, new_content);

        // P1 修复:重新计算 embedding(内容变了,旧 embedding 不再代表语义)
        // 只在 embedder 就绪时更新,否则跳过(enrich_all 后续会补)
        if self.embedder_ready() {
            if let Ok(new_emb) = self.embed(new_content) {
                self.persist_embedding(id, &new_emb);
                self.embeddings.insert(id.to_string(), new_emb);
            }
        }

        Ok(())
    }

    /// 返回完整图数据(给前端 Cytoscape.js 渲染)
    pub fn graph_snapshot(&self) -> serde_json::Value {
        let mut nodes = Vec::new();
        for idx in self.graph.node_indices() {
            let node = &self.graph[idx];
            nodes.push(serde_json::json!({
                "id": node.id,
                "data": {
                    "id": node.id,
                    "title": node.title,
                    "content": node.content,
                    "node_type": node.node_type,
                    "source": node.source,
                    "metadata": node.metadata,
                    "created_at": node.created_at,
                    "updated_at": node.updated_at,
                }
            }));
        }

        let mut edges = Vec::new();
        for edge_ref in self.graph.raw_edges() {
            let s_node = &self.graph[edge_ref.source()];
            let t_node = &self.graph[edge_ref.target()];
            edges.push(serde_json::json!({
                "data": {
                    "id": format!("{}-{}", s_node.id, t_node.id),
                    "source": s_node.id,
                    "target": t_node.id,
                    "relation": edge_ref.weight.relation,
                    "weight": edge_ref.weight.weight,
                }
            }));
        }

        serde_json::json!({
            "nodes": nodes,
            "edges": edges,
            "stats": self.stats(),
        })
    }

    pub fn recent(&self, limit: usize) -> Vec<Node> {
        let mut nodes: Vec<Node> = self.graph.node_indices()
            .map(|i| self.graph[i].clone())
            .collect();
        nodes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        nodes.truncate(limit);
        nodes
    }

    /// 删除节点(连同其所有边)。返回是否删除成功。
    pub fn delete_node(&mut self, id: &str) -> bool {
        if let Some(&idx) = self.node_map.get(id) {
            // 从 petgraph 删除(自动连带删边)
            self.graph.remove_node(idx);
            // node_map 失效(idx 变了),重建
            self.node_map.clear();
            for i in self.graph.node_indices() {
                self.node_map.insert(self.graph[i].id.clone(), i);
            }
            self.embeddings.remove(id);
            // 从 SQLite + FTS5 + embeddings 删
            let _ = self.db.execute("DELETE FROM nodes WHERE id = ?", params![id]);
            let _ = self.db.execute("DELETE FROM edges WHERE source = ? OR target = ?", params![id, id]);
            let _ = self.db.execute("DELETE FROM embeddings WHERE node_id = ?", params![id]);
            let _ = self.db.execute("DELETE FROM node_history WHERE node_id = ?", params![id]);
            self.fts_delete(id);
            true
        } else {
            false
        }
    }

    /// BFS 找 depth 跳邻居(供前端展开节点)。返回 {node, neighbors:[{...node, depth}]}
    pub fn neighbors(&self, id: &str, depth: usize) -> serde_json::Value {
        use petgraph::visit::EdgeRef;
        let start = match self.node_map.get(id) {
            Some(&idx) => idx,
            None => return serde_json::json!({"error": "node not found"}),
        };
        let mut visited = std::collections::HashSet::new();
        visited.insert(start);
        let mut frontier = vec![start];
        let mut neighbors = Vec::new();
        for d in 1..=depth {
            let mut next = Vec::new();
            for &f in &frontier {
                // 出边
                for e in self.graph.edges(f) {
                    let t = e.target();
                    if visited.insert(t) {
                        next.push(t);
                        neighbors.push(serde_json::json!({
                            "id": self.graph[t].id,
                            "title": self.graph[t].title,
                            "content": self.graph[t].content,
                            "node_type": self.graph[t].node_type,
                            "source": self.graph[t].source,
                            "depth": d,
                        }));
                    }
                }
                // 入边
                for e in self.graph.edges_directed(f, petgraph::Direction::Incoming) {
                    let t = e.source();
                    if visited.insert(t) {
                        next.push(t);
                        neighbors.push(serde_json::json!({
                            "id": self.graph[t].id,
                            "title": self.graph[t].title,
                            "content": self.graph[t].content,
                            "node_type": self.graph[t].node_type,
                            "source": self.graph[t].source,
                            "depth": d,
                        }));
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() { break; }
        }
        serde_json::json!({
            "node": serde_json::to_value(&self.graph[start]).unwrap(),
            "neighbors": neighbors,
        })
    }
}

fn is_valid_type(t: &str) -> bool {
    matches!(t, "knowledge" | "preference" | "project" | "fact" | "skill" | "reference")
}
