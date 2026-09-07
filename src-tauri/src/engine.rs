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
const MIN_SIM_THRESHOLD: f32 = 0.3;
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
                metadata TEXT DEFAULT '{}', created_at TEXT, updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS edges (
                source TEXT NOT NULL, target TEXT NOT NULL,
                relation TEXT DEFAULT 'related_to', weight REAL DEFAULT 1.0,
                metadata TEXT DEFAULT '{}', created_at TEXT,
                PRIMARY KEY (source, target)
            );
            CREATE INDEX IF NOT EXISTS idx_node_type ON nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_node_source ON nodes(source);
            CREATE INDEX IF NOT EXISTS idx_node_created ON nodes(created_at);",
        )?;

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

    fn node_id(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())[..16].to_string()
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
        // 模型加载由启动时的后台线程负责(lib.rs setup)。
        // 不在 embed() 里懒加载——避免在 Mutex 锁内触发数分钟的模型下载/加载。
        // embedder 未就绪时直接报错,调用方应先用 embedder_ready() 守卫。
        // RefCell::borrow_mut → RefMut<Option<TextEmbedding>> → as_mut() 拿 &mut TextEmbedding
        let mut guard = self.embedder.borrow_mut();
        let embedder = guard.as_mut().context("Embedding model not loaded yet")?;
        let embeddings = embedder.embed(vec![text.to_string()], None)?;
        Ok(embeddings.into_iter().next().unwrap_or_default())
    }

    fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0. || norm_b == 0. { 0. } else { dot / (norm_a * norm_b) }
    }

    /// 直接写入 SQLite(不生成 embedding,用于批量导入)
    pub fn add_node_raw(&mut self, content: &str, title: &str, node_type: &str, source: &str) -> Result<(), String> {
        let nid = Self::node_id(content);
        let now = Self::now();

        // 跳过已存在(hash 去重)
        if self.node_map.contains_key(&nid) {
            return Ok(());
        }

        let nt = if is_valid_type(node_type) { node_type.to_string() } else { "knowledge".to_string() };
        let t = if title.is_empty() { content.chars().take(40).collect() } else { title.to_string() };
        let node = Node {
            id: nid.clone(), content: content.to_string(), title: t,
            node_type: nt, source: source.to_string(), metadata: "{}".to_string(),
            created_at: now.clone(), updated_at: now,
        };

        self.db.execute(
            "INSERT OR REPLACE INTO nodes VALUES (?,?,?,?,?,?,?,?)",
            params![node.id, node.content, node.title, node.node_type, node.source, node.metadata, node.created_at, node.updated_at],
        ).map_err(|e| e.to_string())?;
        self.fts_upsert(&node.id, &node.title, &node.content);

        let idx = self.graph.add_node(node);
        self.node_map.insert(nid, idx);
        Ok(())
    }

    pub fn add_node(&mut self, content: &str, title: &str, node_type: &str, source: &str, metadata: &str) -> Result<Node> {
        let nid = Self::node_id(content);
        let now = Self::now();

        // 第一层: hash 匹配
        if let Some(&idx) = self.node_map.get(&nid) {
            self.graph[idx].updated_at = now;
            return Ok(self.graph[idx].clone());
        }

        // 模型未就绪时:跳过 embedding 相似度去重,走 raw 写入(hash 去重已覆盖第一层)
        if !self.embedder_ready() {
            return self.add_node_raw_fallback(content, title, node_type, source, metadata);
        }

        // 第二层: embedding 相似度
        let new_emb = self.embed(content)?;
        for (existing_id, existing_emb) in &self.embeddings {
            let sim = Self::cosine_sim(&new_emb, existing_emb);
            if sim >= DEDUP_THRESHOLD {
                if let Some(&idx) = self.node_map.get(existing_id) {
                    self.graph[idx].updated_at = now.clone();
                    return Ok(self.graph[idx].clone());
                }
            }
        }

        // 第三层: 新建
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
            "INSERT OR REPLACE INTO nodes VALUES (?,?,?,?,?,?,?,?)",
            params![node.id, node.content, node.title, node.node_type, node.source, node.metadata, node.created_at, node.updated_at],
        )?;
        self.fts_upsert(&node.id, &node.title, &node.content);

        let idx = self.graph.add_node(node.clone());
        self.node_map.insert(nid.clone(), idx);
        self.embeddings.insert(nid, new_emb);
        Ok(node)
    }

    /// add_node 的降级路径:embedder 未就绪时,跳过 embedding 相似度去重,
    /// 直接 raw 写入(hash 去重已在第一层覆盖)。模型就绪后由 enrich_all 补 embedding。
    fn add_node_raw_fallback(&mut self, content: &str, title: &str, node_type: &str, source: &str, metadata: &str) -> Result<Node> {
        let nt = if is_valid_type(node_type) { node_type.to_string() } else { "knowledge".to_string() };
        let t = if title.is_empty() { content.chars().take(40).collect() } else { title.to_string() };
        let now = Self::now();
        let node = Node {
            id: Self::node_id(content),
            content: content.to_string(),
            title: t,
            node_type: nt,
            source: source.to_string(),
            metadata: if metadata.is_empty() { "{}".to_string() } else { metadata.to_string() },
            created_at: now.clone(),
            updated_at: now,
        };
        self.db.execute(
            "INSERT OR REPLACE INTO nodes VALUES (?,?,?,?,?,?,?,?)",
            params![node.id, node.content, node.title, node.node_type, node.source, node.metadata, node.created_at, node.updated_at],
        )?;
        self.fts_upsert(&node.id, &node.title, &node.content);
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
            if sim < MIN_SIM_THRESHOLD { break; }
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

        // 1. BM25 关键词搜索
        let bm25_results = self.bm25_search(query, top_k * 4);

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
        // 归一化 RRF 到 [0,1](便于后续阈值和融合)
        let max_rrf = rrf_scores.values().cloned().fold(0.0f32, f32::max).max(1e-8);
        let mut sims: Vec<(String, f32)> = rrf_scores.iter()
            .map(|(id, score)| (id.clone(), score / max_rrf))
            .collect();
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        let seeds: Vec<(String, f32)> = sims.iter().take(SEED_TOP_K)
            .filter(|(_, s)| *s >= MIN_SIM_THRESHOLD)
            .cloned()
            .collect();
        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        if !spread {
            return Ok(sims.iter().take(top_k)
                .filter(|(_, s)| *s >= MIN_SIM_THRESHOLD)
                .filter_map(|(id, sim)| {
                    self.node_map.get(id).map(|&idx| {
                        let node = &self.graph[idx];
                        let sem = sem_sims.iter().find(|(sid, _)| sid == id).map(|(_, s)| *s).unwrap_or(0.0);
                        let is_kw = bm25_results.iter().any(|(bid, _)| bid == id);
                        let mt = if is_kw && sem > 0.3 { "hybrid" } else if is_kw { "keyword" } else { "semantic" };
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

        // 4. 图扩散(PageRank 简化版:语义分 + 邻居传播)
        let mut pr_scores: HashMap<String, f64> = HashMap::new();
        let total_seed: f32 = seeds.iter().map(|(_, s)| s).sum();

        for (id, emb) in &self.embeddings {
            let sim = Self::cosine_sim(&query_emb, emb);
            let mut score: f64 = (sim / total_seed) as f64;
            if let Some(&idx) = self.node_map.get(id) {
                for edge_ref in self.graph.edges(idx) {
                    let neighbor_id = &self.graph[edge_ref.target()].id;
                    if let Some(n_sim) = sims.iter().find(|(n_id, _)| n_id == neighbor_id) {
                        score += 0.3 * n_sim.1 as f64 * edge_ref.weight().weight;
                    }
                }
            }
            pr_scores.insert(id.clone(), score);
        }

        let pr_sum: f64 = pr_scores.values().sum();
        let sem_max = sims.first().map(|(_, s)| *s).unwrap_or(1.0);

        let type_priority: HashMap<&str, f64> = [
            ("knowledge", 0.0), ("project", 1.0), ("fact", 2.0),
            ("reference", 3.0), ("preference", 4.0), ("skill", 5.0),
        ].iter().cloned().collect();

        let mut results: Vec<(RetrieveResult, f64)> = self.embeddings.keys()
            .filter_map(|id| {
                // 用 RRF 融合分(而非纯语义分)做阈值和融合
                let rrf = sims.iter().find(|(sid, _)| sid == id).map(|(_, s)| *s).unwrap_or(0.0);
                if rrf < MIN_SIM_THRESHOLD { return None; }
                let sem_orig = sem_sims.iter().find(|(sid, _)| sid == id).map(|(_, s)| *s).unwrap_or(0.0);
                let pr = pr_scores.get(id).copied().unwrap_or(0.0) / pr_sum.max(1e-8);
                let fused = 0.5 * rrf as f64 + 0.5 * pr;
                let idx = self.node_map.get(id)?;
                let node = &self.graph[*idx];
                let priority = type_priority.get(node.node_type.as_str()).copied().unwrap_or(9.0);
                let sort_key = (fused * 100.0).round() / 100.0 - 0.01 * priority;
                let is_kw = bm25_results.iter().any(|(bid, _)| bid == id);
                let mt = if is_kw && sem_orig > 0.3 { "hybrid_spread" } else if is_kw { "keyword_spread" } else { "graph_spread" };
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
        results.truncate(top_k);
        Ok(results.into_iter().map(|(r, _)| r).collect())
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

    /// 更新节点的标题和内容(保留 id, type, source, metadata)
    pub fn update_node_text(&mut self, id: &str, new_title: &str, new_content: &str) -> Result<()> {
        let idx = *self
            .node_map
            .get(id)
            .context("node not found")?;
        let node = &mut self.graph[idx];
        node.title = new_title.to_string();
        node.content = new_content.to_string();
        node.updated_at = chrono::Utc::now().to_rfc3339();
        // 同步到 SQLite + FTS5
        self.db.execute(
            "UPDATE nodes SET title = ?, content = ?, updated_at = ? WHERE id = ?",
            params![new_title, new_content, node.updated_at, id],
        )?;
        self.fts_upsert(id, new_title, new_content);
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
            // 从 SQLite + FTS5 删
            let _ = self.db.execute("DELETE FROM nodes WHERE id = ?", params![id]);
            let _ = self.db.execute("DELETE FROM edges WHERE source = ? OR target = ?", params![id, id]);
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
