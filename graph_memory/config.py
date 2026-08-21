"""Graph Memory — 图式记忆系统配置

所有配置可通过环境变量覆盖,适合开源部署。
复制 .env.example 为 .env 并修改即可。
"""
import os
from pathlib import Path

# 尝试加载 .env 文件(如果 python-dotenv 可用)
try:
    from dotenv import load_dotenv
    load_dotenv(Path(__file__).resolve().parent.parent / ".env")
except ImportError:
    pass

# 项目根目录
BASE_DIR = Path(__file__).resolve().parent.parent

# 数据存储
DATA_DIR = BASE_DIR / "data"
DATA_DIR.mkdir(exist_ok=True)
GRAPH_FILE = DATA_DIR / "graph.json"       # 旧格式(自动迁移后弃用)
GRAPH_DB = DATA_DIR / "graph.db"            # SQLite 持久化
EMBEDDINGS_FILE = DATA_DIR / "embeddings.npz"

# Embedding 模型(本地,不依赖 API)
# 可选: BAAI/bge-base-zh-v1.5 (中文 768维) / all-MiniLM-L6-v2 (英文 384维, 更小)
EMBEDDING_MODEL = os.environ.get("GM_EMBEDDING_MODEL", "BAAI/bge-base-zh-v1.5")

# 服务端口
API_HOST = os.environ.get("GM_HOST", "127.0.0.1")
API_PORT = int(os.environ.get("GM_PORT", "9121"))

# PageRank 参数
PAGERANK_ALPHA = 0.15
PAGERANK_TOL = 1e-6
RETRIEVAL_TOP_K = 5
SEED_TOP_K = 2
MIN_SIM_THRESHOLD = 0.3

# Agent 记忆文件路径(自动检测,可通过环境变量覆盖)
# 默认指向各 agent 在用户主目录下的标准数据位置,跨平台通用。
HERMES_HOME = os.environ.get(
    "HERMES_HOME",
    os.path.join(os.path.expanduser("~"), ".hermes"),
)
CLAUDE_HOME = os.path.join(os.path.expanduser("~"), ".claude")
CODEX_HOME = os.path.join(os.path.expanduser("~"), ".codex")

# LLM 配置(环境变量优先,其次 Hermes config.yaml)
LLM_API_KEY = os.environ.get("GM_LLM_API_KEY", "")
LLM_BASE_URL = os.environ.get("GM_LLM_BASE_URL", "")
LLM_MODEL = os.environ.get("GM_LLM_MODEL", "")
