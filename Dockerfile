# Graph Memory — Dockerfile
# 单容器跑 FastAPI 后端(含可视化前端)。embedding 模型缓存挂卷,避免重复下载。
# 用法:
#   docker build -t graph-memory .
#   docker run -p 9121:9121 -v gm_data:/app/data -v gm_models:/root/.cache/huggingface graph-memory
# 然后访问 http://localhost:9121/

FROM python:3.11-slim

# 系统依赖(sentence-transformers/torch 需要的运行时库)
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 先装依赖(利用层缓存)
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# 再装项目本体
COPY . .
RUN pip install --no-cache-dir -e ".[dotenv]"

# 数据目录(图 + embedding 缓存),由 volume 持久化
RUN mkdir -p /app/data
ENV GM_HOST=0.0.0.0 \
    GM_PORT=9121

# embedding 模型缓存目录(默认 HuggingFace cache)
ENV HF_HOME=/root/.cache/huggingface \
    TRANSFORMERS_OFFLINE=0

EXPOSE 9121

HEALTHCHECK --interval=30s --timeout=10s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,sys; urllib.request.urlopen('http://127.0.0.1:9121/api/health',timeout=5); sys.exit(0)" || exit 1

# start-period 给首次启动下载 embedding 模型留时间
CMD ["python", "-m", "graph_memory.server"]
