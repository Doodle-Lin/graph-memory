"""Graph Memory 演示数据集 —— 首次运行时种入,让空白项目开箱即用。

用法(需先启动 server):
    python seed_demo.py

写入的是通用技术知识样例(无任何个人信息),用于:
  - 验证检索/自动建边/PageRank 扩散是否正常工作
  - 给新用户一个可视化界面上能看到的初始图
  - 配合 benchmark.py / regression.py 做最小可复现回归

清空演示数据: 删除 data/graph.json 和 data/embeddings.npz 后重启 server。
"""
import requests

API = "http://127.0.0.1:9121"

# 通用技术知识样例(无个人信息),覆盖 6 种节点类型 + 自然的关联关系
DEMO_NODES = [
    # knowledge
    {"content": "vLLM 使用 PagedAttention 把 KV Cache 分页管理,减少显存碎片,提升高并发下的 GPU 利用率。",
     "title": "vLLM PagedAttention 原理", "node_type": "knowledge", "source": "demo"},
    {"content": "TensorRT-LLM 需要先编译引擎再推理,延迟更低但部署复杂;vLLM 是 Python 原生,易用性更好但峰值吞吐略低。",
     "title": "vLLM vs TensorRT-LLM", "node_type": "knowledge", "source": "demo"},
    {"content": "PagedAttention 借鉴操作系统的虚拟内存分页思想,KV Cache 按固定大小 block 分配,按需映射到物理显存。",
     "title": "PagedAttention 分页思想", "node_type": "knowledge", "source": "demo"},
    # project
    {"content": "示例推理服务部署在 GPU 服务器,对外端口 8000,使用 vLLM 后端,模型为某开源 7B 模型。",
     "title": "示例推理服务部署", "node_type": "project", "source": "demo"},
    # fact
    {"content": "跨服务器传输 Docker 镜像推荐用 docker save | gzip 导出 → scp 中转 → docker load 导入,而非在目标机直接 build(慢且需要完整构建上下文)。",
     "title": "Docker 跨服务器传输", "node_type": "fact", "source": "demo"},
    {"content": "ComfyUI fp8 推理配置: dtype bf16 + weight_dtype=fp8_e4m3fn,相比纯 bf16 可节省约 50% 显存,精度损失可接受。",
     "title": "ComfyUI fp8 配置", "node_type": "fact", "source": "demo"},
    # skill
    {"content": "ComfyUI 启动自定义模型路径与挂载: 通过 --base-path 指定,或软链 models 目录到 ComfyUI 根下。",
     "title": "ComfyUI 模型路径配置", "node_type": "skill", "source": "demo"},
    # preference
    {"content": "用户偏好先跑通最小可用版本再优化,避免在未验证可行性的方案上过度投入。",
     "title": "先跑通再优化", "node_type": "preference", "source": "demo"},
    {"content": "用户偏好实验代码每步及时 commit,实验在 feature 分支进行,确认收益后再合入 main。",
     "title": "实验代码 Git 管理", "node_type": "preference", "source": "demo"},
    # reference
    {"content": "DiffSynth-Studio 是一个支持 LoRA 微调的训练框架,提供 train 脚本和 Dockerfile 容器化方案。",
     "title": "DiffSynth-Studio 训练框架", "node_type": "reference", "source": "demo"},
    {"content": "内网开发受限时: HF 不可达可用 ModelScope, GitHub 不可达可用 gitee 镜像, Docker 镜像可用国内加速源。",
     "title": "内网镜像源替代", "node_type": "reference", "source": "demo"},
]


def main():
    print("种入演示数据集...")
    stats_before = requests.get(f"{API}/api/stats", timeout=10).json()
    print(f"  当前图: {stats_before['node_count']} 节点 / {stats_before['edge_count']} 边")

    created = 0
    for n in DEMO_NODES:
        try:
            r = requests.post(f"{API}/api/write", json={
                "content": n["content"], "title": n["title"],
                "node_type": n["node_type"], "source": n["source"],
                "auto_link": True, "max_links": 5,
            }, timeout=120)
            if r.ok:
                created += 1
                print(f"  ✓ {n['title']}")
            else:
                print(f"  ✗ 写入失败: {n['title']} → {r.status_code}")
        except Exception as e:
            print(f"  ✗ 写入异常: {n['title']} → {e}")

    stats_after = requests.get(f"{API}/api/stats", timeout=10).json()
    print(f"  写入 {created} 个节点")
    print(f"  现在图: {stats_after['node_count']} 节点 / {stats_after['edge_count']} 边")
    print(f"  自动建边 {stats_after['edge_count'] - stats_before['edge_count']} 条")
    print("打开 http://127.0.0.1:9121/ 查看可视化界面。")


if __name__ == "__main__":
    main()
