"""Graph Memory — 记忆导入器

从各 Agent 的记忆文件中解析知识,导入图引擎。
每个记忆条目 → 一个知识节点,自动建边形成初始图谱。

支持的来源(均通过环境变量定位路径,见 config.py):
  - Hermes 桌面端 / CLI (HERMES_HOME, 默认 ~/.hermes)
  - Claude Code (.claude/projects/.../memory/*.md, CLAUDE_HOME 默认 ~/.claude)
  - Codex (~/.codex, history.jsonl + memories_1.sqlite)
"""
from __future__ import annotations

import os
import re
from pathlib import Path
from typing import Any

from .config import HERMES_HOME, CLAUDE_HOME, CODEX_HOME

# Hermes CLI / 桌面端 共用 ~/.hermes 作为默认根(可通过 HERMES_HOME 覆盖)
HERMES_CLI_HOME = os.path.join(os.path.expanduser("~"), ".hermes")

from .engine import GraphEngine

# ── 质量过滤 ────────────────────────────────────────────────
# 对话碎片/无意义内容过滤。不靠长尾关键词黑名单(会误杀好节点),
# 而是靠内容长度 + 信息密度判断。只保留普适的语气词/斜杠指令/系统噪声前缀。

# 绝对无信息量的短文本(纯语气词/碎片指令),跨用户通用
_NOISE_PATTERNS = re.compile(
    r'^(ok|okay|好的|嗯|收到|明白|了解|继续|跳过|不懂|不会|在吗|在的|'
    r'/model|/help|/clear|/exit|/quit|/reset|/new|/config|'
    r'继续|下一步|为啥|怎么|为什么|跑个例子|启动服务)'
    r'\s*[？?]?\s*$',
    re.IGNORECASE
)

# 噪声前缀(出现在内容开头则跳过)
_NOISE_PREFIXES = (
    "Launching skill:", "Exit code", "Base directory",
    ".claude\\skills\\",  # Claude skills 路径前缀
    "To support symlinks", "UserWarning:",
    "[TOOL]", "[Mock]", "You said:",
    "run_command", "Error: command",
    "07:", "08:", "09:", "10:", "11:",  # 时间戳开头的对话片段
)

# 噪声内容检测(不只是前缀)
def _is_noise(text: str, min_length: int = 40) -> bool:
    """检测是否为噪声内容(对话片段/配置片段/时间戳等)"""
    if len(text) < min_length:
        return True
    # JSON 配置片段
    if text.strip().startswith("{") and ("env" in text[:100].lower() or "model" in text[:100].lower()):
        return True
    # 短 git 命令片段
    if text.strip().startswith("git ") and len(text) < 50:
        return True
    # 包含对话标记
    for marker in ("[TOOL]", "[Mock]", "You said:", "run_command"):
        if marker in text:
            return True
    # 时间戳开头
    if text[:2].isdigit() and ":" in text[:8]:
        return True
    for prefix in _NOISE_PREFIXES:
        if text.startswith(prefix):
            return True
    # 去掉标点/空白后,有效字符 < 15 → 噪声
    cleaned = re.sub(r'[\s\W_]+', '', text)
    if len(cleaned) < 15:
        return True
    return False


def _quality_filter(text: str, source: str = "", min_length: int = 40) -> bool:
    """质量过滤:返回 True 表示通过(保留),False 表示过滤掉

    对不同来源用不同阈值:
    - hermes/claude memory .md: 内容已经过整理,阈值低(20字)
    - codex history / claude session: 对话碎片多,阈值高(50字)
    - skill: 技能描述,阈值低(20字)
    """
    thresholds = {
        "codex": 50, "claude_session": 50, "hermes_cli": 50,
        "hermes_session": 50,
        "hermes": 15, "claude": 15, "skill": 15,  # 已整理的记忆,低阈值
    }
    threshold = thresholds.get(source, min_length)
    return not _is_noise(text, min_length=threshold)


def _split_hermes_memory(content: str) -> list[dict]:
    """Hermes MEMORY.md / USER.md: § 分隔条目"""
    entries = []
    # § 是分隔符,每段一条记忆
    parts = re.split(r'\n§\n', content)
    for part in parts:
        part = part.strip()
        if not part or len(part) < 5:
            continue
        # 格式: "key:value" 或自由文本
        title = ""
        body = part
        if ":" in part.split("\n")[0]:
            first_line = part.split("\n")[0]
            title = first_line.split(":")[0].strip()
            body = part
        entries.append({"title": title, "content": body, "node_type": "knowledge"})
    return entries


def _parse_claude_md(filepath: Path) -> dict | None:
    """Claude Code memory .md: YAML frontmatter + markdown body"""
    text = filepath.read_text(encoding="utf-8", errors="replace")
    # 解析 YAML frontmatter
    if text.startswith("---"):
        parts = text.split("---", 2)
        if len(parts) >= 3:
            fm = parts[1]
            body = parts[2].strip()
            meta = {}
            for line in fm.strip().split("\n"):
                if ":" in line:
                    k, v = line.split(":", 1)
                    meta[k.strip()] = v.strip()
            # 类型映射:旧类型 → 新 6 类
            raw_type = meta.get("type", "knowledge")
            type_map = {
                "user": "preference",
                "feedback": "reference",
                "memory": "knowledge",
                "entity": "knowledge",
                "history": "knowledge",
                "session": "knowledge",
            }
            mapped_type = type_map.get(raw_type, raw_type)
            if mapped_type not in ("knowledge", "preference", "project", "fact", "skill", "reference"):
                mapped_type = "knowledge"
            return {
                "title": meta.get("name", filepath.stem),
                "content": body,
                "node_type": mapped_type,
                "source": "claude",
                "metadata": {"file": filepath.name, "original_type": raw_type, **meta},
            }
    return {
        "title": filepath.stem,
        "content": text[:2000],
        "node_type": "knowledge",
        "source": "claude",
        "metadata": {"file": filepath.name},
    }


def import_hermes(engine: GraphEngine) -> dict:
    """导入 Hermes 记忆(MEMORY.md + USER.md 等,位于 HERMES_HOME/memories/)"""
    imported = {"source": "hermes", "nodes": 0, "details": [], "errors": []}

    mem_dir = Path(HERMES_HOME) / "memories"
    if not mem_dir.exists():
        imported["error"] = f"Directory not found: {mem_dir}"
        return imported

    for md_file in mem_dir.glob("*.md"):
        try:
            content = md_file.read_text(encoding="utf-8", errors="replace")
        except Exception as e:
            imported["errors"].append({"file": str(md_file), "error": str(e)})
            continue
        entries = _split_hermes_memory(content)
        for entry in entries:
            source_tag = "hermes_user" if "USER" in md_file.name else "hermes"
            node = engine.add_node(
                content=entry["content"],
                title=entry["title"] or md_file.stem,
                node_type="preference" if "USER" in md_file.name else "knowledge",
                source=source_tag,
                metadata={"file": md_file.name},
            )
            if node:
                links = engine.auto_link(node["id"], max_links=3)
                imported["nodes"] += 1
                imported["details"].append({
                    "id": node["id"], "title": node["title"],
                    "links": len(links),
                })
    return imported


def import_claude_code(engine: GraphEngine) -> dict:
    """导入 Claude Code 记忆(.claude/projects/*/memory/*.md)"""
    imported = {"source": "claude_code", "nodes": 0, "details": []}

    projects_dir = Path(CLAUDE_HOME) / "projects"
    if not projects_dir.exists():
        imported["error"] = f"Directory not found: {projects_dir}"
        return imported

    # 递归找所有 memory/*.md
    md_files = list(projects_dir.rglob("memory/*.md"))
    if not md_files:
        # 也搜索顶层 memory 文件
        md_files = list(projects_dir.rglob("*.md"))

    for md_file in md_files:
        parsed = _parse_claude_md(md_file)
        if parsed:
            node = engine.add_node(
                content=parsed["content"],
                title=parsed["title"],
                node_type=parsed["node_type"],
                source="claude",
                metadata=parsed.get("metadata", {}),
            )
            if node:
                links = engine.auto_link(node["id"], max_links=5)
                imported["nodes"] += 1
                imported["details"].append({
                    "id": node["id"], "title": node["title"],
                    "links": len(links),
                })
    return imported


def import_codex(engine: GraphEngine) -> dict:
    """导入 Codex 记忆(.codex/history.jsonl + memories_1.sqlite)"""
    imported = {"source": "codex", "nodes": 0, "details": [], "errors": []}

    # history.jsonl
    hist_file = Path(CODEX_HOME) / "history.jsonl"
    if hist_file.exists():
        import json
        lines = hist_file.read_text(encoding="utf-8", errors="replace").strip().split("\n")
        for line in lines:
            if not line.strip():
                continue
            try:
                entry = json.loads(line)
                text = entry.get("text", "")
                if not text or _quality_filter(text, source="codex") is False:
                    continue
                node = engine.add_node(
                    content=text,
                    title=text[:40],
                    node_type="history",
                    source="codex",
                    metadata={"session_id": entry.get("session_id", "")},
                )
                if node:
                    links = engine.auto_link(node["id"], max_links=3)
                    imported["nodes"] += 1
                    imported["details"].append({
                        "id": node["id"], "title": node["title"],
                        "links": len(links),
                    })
            except Exception as e:
                imported["errors"].append({"file": str(hist_file), "error": str(e)})

    # memories SQLite
    db_file = Path(CODEX_HOME) / "memories_1.sqlite"
    if db_file.exists():
        import sqlite3
        try:
            conn = sqlite3.connect(str(db_file))
            cursor = conn.cursor()
            cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
            tables = [t[0] for t in cursor.fetchall()]
            if "stage1_outputs" in tables:
                cursor.execute("SELECT raw_memory, rollout_summary FROM stage1_outputs")
                for row in cursor.fetchall():
                    content = row[0] or row[1] or ""
                    if content and _quality_filter(content, source="codex"):
                        node = engine.add_node(
                            content=content,
                            title=content[:40],
                            node_type="memory",
                            source="codex",
                        )
                        if node:
                            links = engine.auto_link(node["id"], max_links=5)
                            imported["nodes"] += 1
                            imported["details"].append({
                                "id": node["id"], "title": node["title"],
                                "links": len(links),
                            })
            conn.close()
        except Exception as e:
            imported["error"] = f"SQLite read error: {e}"

    return imported


def import_hermes_skills(engine: GraphEngine) -> dict:
    """导入 Hermes skills 列表作为知识节点"""
    imported = {"source": "hermes_skills", "nodes": 0, "details": []}
    skills_dir = Path(HERMES_HOME) / "skills"
    if not skills_dir.exists():
        imported["error"] = f"Directory not found: {skills_dir}"
        return imported

    # 找所有 SKILL.md
    for skill_md in skills_dir.rglob("SKILL.md"):
        try:
            text = skill_md.read_text(encoding="utf-8", errors="replace")
            # 提取 name 和 description 从 frontmatter
            name = skill_md.parent.name
            desc = ""
            if text.startswith("---"):
                parts = text.split("---", 2)
                if len(parts) >= 2:
                    for line in parts[1].split("\n"):
                        if line.strip().startswith("name:"):
                            name = line.split(":", 1)[1].strip()
                        if line.strip().startswith("description:"):
                            desc = line.split(":", 1)[1].strip()
            content = desc or text[:500]
            node = engine.add_node(
                content=content,
                title=name,
                node_type="skill",
                source="hermes_skill",
                metadata={"path": str(skill_md.parent)},
            )
            if node:
                links = engine.auto_link(node["id"], max_links=5)
                imported["nodes"] += 1
                imported["details"].append({
                    "id": node["id"], "title": name, "links": len(links),
                })
        except Exception as e:
            imported.setdefault("errors", []).append({"file": str(skill_md), "error": str(e)})

    return imported


def import_claude_sessions(engine: GraphEngine) -> dict:
    """导入 Claude Code 会话 JSONL(用户提问 + assistant 文本回复)

    106 个 session, 24455 行。只提取有知识价值的文本,跳过 tool_use/tool_result。
    每个 session 提取为一个摘要节点 + 用户提问片段。
    """
    imported = {"source": "claude_sessions", "nodes": 0, "details": [], "errors": []}

    projects_dir = Path(CLAUDE_HOME) / "projects"
    if not projects_dir.exists():
        imported["error"] = f"Directory not found: {projects_dir}"
        return imported

    jsonl_files = list(projects_dir.rglob("*.jsonl"))
    imported["total_sessions"] = len(jsonl_files)

    for jf in jsonl_files:
        try:
            session_id = jf.stem
            user_messages = []
            assistant_texts = []
            session_title = ""

            with open(jf, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        obj = json.loads(line)
                    except Exception as e:
                        imported["errors"].append({"file": str(jf), "line": "parse", "error": str(e)})
                        continue

                    if obj.get("type") == "ai-title":
                        session_title = obj.get("title", "")

                    elif obj.get("type") == "user":
                        msg = obj.get("message", {})
                        if isinstance(msg, dict):
                            content = msg.get("content", "")
                            if isinstance(content, str) and len(content) > 10:
                                # 跳过 tool_result 和 skill launch
                                if (not content.startswith("Launching skill:")
                                   and not content.startswith("Exit code")
                                   and not content.startswith("Base directory")):
                                    user_messages.append(content[:500])
                            elif isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict) and block.get("type") == "text":
                                        text = block.get("text", "")
                                        if len(text) > 10:
                                            user_messages.append(text[:500])

                    elif obj.get("type") == "assistant":
                        msg = obj.get("message", {})
                        if isinstance(msg, dict):
                            content = msg.get("content", [])
                            if isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict) and block.get("type") == "text":
                                        text = block.get("text", "")
                                        if len(text) > 30:
                                            assistant_texts.append(text[:800])

            # 只为有实质内容的 session 创建节点
            if not user_messages and not assistant_texts:
                continue

            # 用户提问合并为一个节点
            if user_messages:
                user_content = "\n---\n".join(user_messages[:10])  # 最多10条
                title = session_title or user_messages[0][:50]
                node = engine.add_node(
                    content=user_content,
                    title=title,
                    node_type="session",
                    source="claude_session",
                    metadata={"session_id": session_id, "role": "user",
                              "msg_count": len(user_messages)},
                )
                if node:
                    links = engine.auto_link(node["id"], max_links=5)
                    imported["nodes"] += 1
                    imported["details"].append({
                        "id": node["id"], "title": title,
                        "links": len(links),
                    })

            # Assistant 回复中较长的作为知识节点(可能含技术方案)
            for i, text in enumerate(assistant_texts[:5]):  # 每session最多5条
                if len(text) < 50:
                    continue
                node = engine.add_node(
                    content=text,
                    title=text[:50],
                    node_type="knowledge",
                    source="claude_session",
                    metadata={"session_id": session_id, "role": "assistant"},
                )
                if node:
                    links = engine.auto_link(node["id"], max_links=5)
                    imported["nodes"] += 1
                    imported["details"].append({
                        "id": node["id"], "title": node["title"],
                        "links": len(links),
                    })

        except Exception as e:
            imported["errors"].append({"file": str(jf), "error": str(e)})

    return imported


def import_hermes_cli(engine: GraphEngine) -> dict:
    """导入 Hermes CLI (~/.hermes) 的对话历史和技能

    .hermes_history: # 时间戳 格式的对话历史
    skills/: CLI 侧安装的技能
    """
    imported = {"source": "hermes_cli", "nodes": 0, "details": []}

    cli_home = Path(HERMES_CLI_HOME)
    if not cli_home.exists():
        imported["error"] = f"Directory not found: {cli_home}"
        return imported

    # 1. 对话历史
    hist_file = cli_home / ".hermes_history"
    if hist_file.exists():
        content = hist_file.read_text(encoding="utf-8", errors="replace")
        # 按时间戳分组: # 2026-08-12 07:49:44.280\n+内容
        entries = re.split(r"\n# \d{4}-\d{2}-\d{2}", content)
        for entry in entries:
            entry = entry.strip()
            if not entry or len(entry) < 5:
                continue
            # 去掉时间戳前缀
            text = re.sub(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d+\n", "", entry)
            text = text.lstrip("+").strip()
            if len(text) < 3:
                continue
            node = engine.add_node(
                content=text,
                title=text[:40],
                node_type="history",
                source="hermes_cli",
                metadata={"file": ".hermes_history"},
            )
            if node:
                links = engine.auto_link(node["id"], max_links=3)
                imported["nodes"] += 1
                imported["details"].append({
                    "id": node["id"], "title": node["title"],
                    "links": len(links),
                })

    # 2. CLI 技能
    cli_skills = cli_home / "skills"
    if cli_skills.exists():
        for skill_md in cli_skills.rglob("SKILL.md"):
            try:
                text = skill_md.read_text(encoding="utf-8", errors="replace")
                name = skill_md.parent.name
                desc = ""
                if text.startswith("---"):
                    parts = text.split("---", 2)
                    if len(parts) >= 2:
                        for line in parts[1].split("\n"):
                            if line.strip().startswith("name:"):
                                name = line.split(":", 1)[1].strip()
                            if line.strip().startswith("description:"):
                                desc = line.split(":", 1)[1].strip()
                content = desc or text[:500]
                node = engine.add_node(
                    content=content,
                    title=name,
                    node_type="skill",
                    source="hermes_cli_skill",
                    metadata={"path": str(skill_md.parent)},
                )
                if node:
                    links = engine.auto_link(node["id"], max_links=5)
                    imported["nodes"] += 1
                    imported["details"].append({
                        "id": node["id"], "title": name, "links": len(links),
                    })
            except Exception as e:
                imported.setdefault("errors", []).append({"file": str(skill_md), "error": str(e)})

    # 3. Hermes Desktop sessions (request_dump)
    desktop_sessions = Path(HERMES_HOME) / "sessions"
    if desktop_sessions.exists():
        import json
        for sf in desktop_sessions.glob("*.json"):
            try:
                data = json.loads(sf.read_text(encoding="utf-8", errors="replace"))
                # 提取对话内容
                messages = data.get("messages", data.get("conversation", []))
                if isinstance(messages, list):
                    for msg in messages:
                        if isinstance(msg, dict):
                            role = msg.get("role", "")
                            content = msg.get("content", "")
                            if isinstance(content, str) and len(content) > 20 and role in ("user", "assistant"):
                                node = engine.add_node(
                                    content=content[:2000],
                                    title=content[:50],
                                    node_type="session",
                                    source="hermes_session",
                                    metadata={"role": role, "file": sf.name},
                                )
                                if node:
                                    links = engine.auto_link(node["id"], max_links=3)
                                    imported["nodes"] += 1
                                    imported["details"].append({
                                        "id": node["id"], "title": node["title"],
                                        "links": len(links),
                                    })
            except Exception as e:
                imported.setdefault("errors", []).append({"file": str(sf), "error": str(e)})

    return imported


def import_all(engine: GraphEngine) -> dict:
    """导入所有来源的记忆

    结构化记忆(.md memory files, skills)直接导入 — 它们已经整理过。
    对话记录(session jsonl, history)不走这里 — 由 LLM 提炼接口处理。
    """
    results = {
        "hermes_desktop": import_hermes(engine),
        "claude_code_memory": import_claude_code(engine),
        "codex_sqlite": _import_codex_sqlite(engine),
        "hermes_desktop_skills": import_hermes_skills(engine),
    }
    total_nodes = sum(r.get("nodes", 0) for r in results.values())
    return {"total_nodes": total_nodes, "sources": results}


def _import_codex_sqlite(engine: GraphEngine) -> dict:
    """只导入 Codex SQLite 记忆(已提炼的),跳过 history.jsonl(对话碎片)"""
    import sqlite3
    imported = {"source": "codex_sqlite", "nodes": 0, "details": []}

    db_file = Path(CODEX_HOME) / "memories_1.sqlite"
    if not db_file.exists():
        imported["error"] = f"File not found: {db_file}"
        return imported

    try:
        conn = sqlite3.connect(str(db_file))
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = [t[0] for t in cursor.fetchall()]
        if "stage1_outputs" in tables:
            cursor.execute("SELECT raw_memory, rollout_summary FROM stage1_outputs")
            for row in cursor.fetchall():
                content = row[0] or row[1] or ""
                if content and _quality_filter(content, source="codex"):
                    node = engine.add_node(
                        content=content,
                        title=content[:40],
                        node_type="memory",
                        source="codex",
                    )
                    if node:
                        links = engine.auto_link(node["id"], max_links=5)
                        imported["nodes"] += 1
                        imported["details"].append({
                            "id": node["id"], "title": node["title"],
                            "links": len(links),
                        })
        conn.close()
    except Exception as e:
        imported["error"] = f"SQLite read error: {e}"

    return imported
