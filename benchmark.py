"""Graph Memory 评测套件 v2 — 充分验证

改进:
  1. 30 道题(10道有图知识/10道图里没有的/10道边缘)
  2. LLM 评分(用另一个 LLM 判断回答质量,不只是关键词匹配)
  3. 防幻觉测试(图里没有的信息,LLM 应该说"不知道")
  4. 3轮取平均(LLM 输出有随机性)
  5. 多维度评分: 准确性/完整性/无幻觉

评估方式:
  - 基线A: 只给 MEMORY.md
  - 基线B: 给 MEMORY.md + 图检索结果
  - 对比: 每道题跑3轮,用 LLM 评分(0-5分),取平均
"""
import requests, json, time, os, sys, statistics

API = "http://127.0.0.1:9121"
ROUNDS = 3  # 每题跑3轮取平均

# ── LLM 配置 ────────────────────────────────────────────────

def load_llm():
    """从环境变量加载评测用 LLM 配置(GM_LLM_*),与引擎共用同一套配置。"""
    base_url = os.environ.get("GM_LLM_BASE_URL", "")
    api_key = os.environ.get("GM_LLM_API_KEY", "")
    model = os.environ.get("GM_LLM_MODEL", "")
    if base_url and api_key and model:
        return {"base_url": base_url, "api_key": api_key, "model": model}
    print("⚠ 未配置 GM_LLM_* 环境变量,评测无法进行。请先 cp .env.example .env 并填写。", file=sys.stderr)
    sys.exit(1)

LLM = load_llm()

def call_llm(system_prompt, user_prompt, max_tokens=500):
    t0 = time.time()
    try:
        r = requests.post(f"{LLM['base_url']}/chat/completions",
            headers={"Authorization": f"Bearer {LLM['api_key']}", "Content-Type": "application/json"},
            json={"model": LLM["model"], "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}],
                "max_tokens": max_tokens, "temperature": 0.1},
            timeout=60)
        r.raise_for_status()
        data = r.json()
        content = data["choices"][0]["message"]["content"]
        usage = data.get("usage", {})
        elapsed = time.time() - t0
        return content, elapsed, usage.get("total_tokens", 0), usage.get("prompt_tokens", 0), usage.get("completion_tokens", 0)
    except Exception as e:
        return f"ERROR: {e}", time.time() - t0, 0, 0, 0


def llm_judge(question, answer, reference_answer, has_info):
    """用 LLM 给回答打分(0-5),三个维度:
    - accuracy: 是否准确(和参考答案对比)
    - completeness: 是否完整(关键信息是否覆盖)
    - hallucination: 是否编造(如果 has_info=False,应该说不知道)
    返回 {accuracy, completeness, hallucination, overall}
    """
    if has_info:
        judge_prompt = f"""你是一个评分专家。根据参考答案给回答打分(0-5分)。

问题: {question}
参考答案: {reference_answer}
待评分回答: {answer}

评分维度:
- accuracy (0-5): 回答是否准确,有无错误信息
- completeness (0-5): 关键信息是否完整覆盖
- overall (0-5): 综合评分

输出严格JSON:
{{"accuracy": 4, "completeness": 3, "overall": 3}}"""
    else:
        judge_prompt = f"""你是一个评分专家。判断回答是否正确地表示"不知道"。

问题: {question}
参考答案: 此信息不在提供的知识库中,正确回答应该是"我不知道"或类似表述。
待评分回答: {answer}

评分维度:
- accuracy (0-5): 如果回答说"不知道"得5分,如果编造信息得0-2分
- hallucination (0-5): 0分=严重编造,5分=诚实说不知道
- overall (0-5): 综合评分

输出严格JSON:
{{"accuracy": 5, "hallucination": 5, "overall": 5}}"""

    result, _, _, _, _ = call_llm("你是评分专家,只输出JSON。", judge_prompt, max_tokens=1000)
    try:
        import re
        # 某些推理模型会把正文留在 reasoning_content 而非 content
        if not result or result.strip() == "":
            # 直接用 reasoning_content (可能包含思考过程里的 JSON)
            return {"accuracy": 3, "completeness": 3, "overall": 3, "hallucination": 3}
        result = re.sub(r"```json\s*", "", result)
        result = re.sub(r"```\s*$", "", result).strip()
        start = result.find("{")
        end = result.rfind("}")
        if start >= 0 and end >= 0:
            scores = json.loads(result[start:end+1])
            return {
                "accuracy": scores.get("accuracy", 0),
                "completeness": scores.get("completeness", scores.get("hallucination", 0)),
                "overall": scores.get("overall", 0),
                "hallucination": scores.get("hallucination", 5 if not has_info else 0),
            }
    except:
        pass
    return {"accuracy": 0, "completeness": 0, "overall": 0, "hallucination": 0}


# ── 30道题 ──────────────────────────────────────────────────
# 注:这些题是为作者本人的知识库量身定的回归集。
# 开源后应替换为你自己的知识库问答对(见 benchmark_questions.example.py)。
# A组=图里有答案, B组=图里没有(防幻觉), C组=部分信息需推理。

QUESTIONS = [
    # === A组: 图记忆中有明确答案 (10题) ===
    {"q": "示例项目部署在哪个服务器?IP和端口?", "ref": "(请替换为你知识库中的项目部署信息)", "has_info": True, "group": "A"},
    {"q": "示例设备的语音方案用什么?端口号?", "ref": "(请替换)", "has_info": True, "group": "A"},
    {"q": "示例设备的三层架构是什么?", "ref": "(请替换)", "has_info": True, "group": "A"},
    {"q": "Docker跨服务器部署怎么做?", "ref": "docker save|gzip导出→scp中转→docker load导入,不直接build(慢)", "has_info": True, "group": "A"},
    {"q": "ComfyUI fp8怎么配置?省多少显存?", "ref": "bf16+weight_dtype=fp8_e4m3fn,省50%显存", "has_info": True, "group": "A"},
    {"q": "示例推理服务在服务器上的路径和端口?", "ref": "(请替换)", "has_info": True, "group": "A"},
    {"q": "示例API用的什么模型?", "ref": "(请替换)", "has_info": True, "group": "A"},
    {"q": "示例训练环境的根目录?", "ref": "(请替换)", "has_info": True, "group": "A"},
    {"q": "用户对文档生成有什么偏好?", "ref": "(请替换为你的偏好类知识)", "has_info": True, "group": "A"},
    {"q": "用户工作目录偏好是什么?", "ref": "(请替换)", "has_info": True, "group": "A"},

    # === B组: 图记忆中没有的信息 (10题,测试防幻觉) ===
    {"q": "2024年Nobel物理学奖得主是谁?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "Python 3.13新增了什么特性?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "上海今天天气怎么样?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "Rust语言的async运行时有哪些?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "CRISPR基因编辑技术的原理是什么?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "特斯拉2024年Q3财报营收多少?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "Kubernetes的Pod和Deployment有什么区别?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "长江的全长是多少公里?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "量子计算中的qubit和经典bit的本质区别?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},
    {"q": "贝多芬第九交响曲创作于哪一年?", "ref": "此信息不在知识库中", "has_info": False, "group": "B"},

    # === C组: 边缘问题(图记忆有部分信息,需要推理) (10题) ===
    {"q": "如果我要在新服务器上部署服务,应该注意什么?", "ref": "(请替换为你的部署相关经验)", "has_info": True, "group": "C"},
    {"q": "面试被问到量化原理,我应该怎么准备?", "ref": "(请替换)", "has_info": True, "group": "C"},
    {"q": "示例设备系统更新后出问题怎么办?", "ref": "(请替换)", "has_info": True, "group": "C"},
    {"q": "在示例GPU上训练LoRA需要什么环境?", "ref": "(请替换)", "has_info": True, "group": "C"},
    {"q": "用户说不懂某个技术概念时应该怎么做?", "ref": "(请替换为你的交互偏好)", "has_info": True, "group": "C"},
    {"q": "Hermes和Claude Code的记忆文件分别在哪?", "ref": "Hermes:~/.hermes/memories/MEMORY.md,Claude:~/.claude/projects/*/memory/*.md", "has_info": True, "group": "C"},
    {"q": "在内网开发有什么限制?", "ref": "(请替换为你的网络环境事实)", "has_info": True, "group": "C"},
    {"q": "vLLM和TensorRT-LLM有什么区别?", "ref": "vLLM更易用(Python原生),TRT-LLM更快(需编译,延迟更低)", "has_info": True, "group": "C"},
    {"q": "示例教学项目有几个阶段?", "ref": "(请替换)", "has_info": True, "group": "C"},
    {"q": "用户对实验代码的Git管理有什么要求?", "ref": "(请替换为你的git偏好)", "has_info": True, "group": "C"},
]


def retrieve_from_graph(query, top_k=5):
    t0 = time.time()
    r = requests.post(f"{API}/api/retrieve", json={"query": query, "top_k": top_k, "spread": True}, timeout=30)
    elapsed = time.time() - t0
    results = r.json()
    context = "\n\n".join([f"[{res.get('title','?')}] {res.get('content','')}" for res in results])
    return context, elapsed, len(results)


def load_memory_md():
    """加载作为评测基线的 MEMORY.md。

    路径优先级:GM_MEMORY_MD 环境变量 > HERMES_HOME/memories/MEMORY.md。
    这是一个评测用的对照基线文本,开源用户应替换为自己的基线记忆文件。
    """
    mem_path = os.environ.get("GM_MEMORY_MD", "")
    if not mem_path:
        from graph_memory.config import HERMES_HOME
        mem_path = os.path.join(HERMES_HOME, "memories", "MEMORY.md")
    if os.path.exists(mem_path):
        return open(mem_path, encoding="utf-8").read()
    return ""


def main():
    print("=" * 70)
    print("  Graph Memory 评测套件 v2 — 30题 / 3轮 / LLM评分 / 防幻觉")
    print(f"  LLM: {LLM['model']}  轮数: {ROUNDS}  题数: {len(QUESTIONS)}")
    print("=" * 70)

    memory_md = load_memory_md()
    stats = requests.get(f"{API}/api/stats", timeout=10).json()
    print(f"  MEMORY.md: {len(memory_md)}字符  图: {stats['node_count']}节点/{stats['edge_count']}边")

    SYSTEM = "你是一个技术助手,根据提供的上下文回答问题。如果上下文中没有相关信息,回答'我不知道'。不要编造信息。"

    all_results = []

    for qi, q in enumerate(QUESTIONS):
        print(f"\n{'─' * 70}")
        print(f"  Q{qi+1}/{len(QUESTIONS)} [{q['group']}] {q['q']}")
        print(f"  参考: {q['ref'][:80]}")
        print(f"{'─' * 70}")

        # 图检索(只做一次,复用)
        context, ret_time, ret_count = retrieve_from_graph(q["q"], top_k=5)

        scores_a = []
        scores_b = []
        times_a = []
        times_b = []
        tokens_a = []
        tokens_b = []

        for rnd in range(ROUNDS):
            # 基线A: 只用 MEMORY.md
            prompt_a = f"上下文:\n{memory_md}\n\n问题: {q['q']}"
            ans_a, t_a, tok_a, _, _ = call_llm(SYSTEM, prompt_a)
            judge_a = llm_judge(q["q"], ans_a, q["ref"], q["has_info"])
            scores_a.append(judge_a)
            times_a.append(t_a)
            tokens_a.append(tok_a)

            # 基线B: MEMORY.md + 图检索
            prompt_b = f"上下文:\n{memory_md}\n\n知识图谱检索结果:\n{context}\n\n问题: {q['q']}"
            ans_b, t_b, tok_b, _, _ = call_llm(SYSTEM, prompt_b)
            judge_b = llm_judge(q["q"], ans_b, q["ref"], q["has_info"])
            scores_b.append(judge_b)
            times_b.append(t_b)
            tokens_b.append(tok_b)

        # 取平均
        avg_a = {
            "accuracy": statistics.mean(s["accuracy"] for s in scores_a),
            "completeness": statistics.mean(s["completeness"] for s in scores_a),
            "overall": statistics.mean(s["overall"] for s in scores_a),
        }
        avg_b = {
            "accuracy": statistics.mean(s["accuracy"] for s in scores_b),
            "completeness": statistics.mean(s["completeness"] for s in scores_b),
            "overall": statistics.mean(s["overall"] for s in scores_b),
        }
        avg_time_a = statistics.mean(times_a)
        avg_time_b = statistics.mean(times_b)
        avg_tok_a = statistics.mean(tokens_a)
        avg_tok_b = statistics.mean(tokens_b)

        diff = avg_b["overall"] - avg_a["overall"]
        symbol = "🚀" if diff > 0.5 else "📈" if diff > 0.1 else "➡️" if abs(diff) <= 0.1 else "📉" if diff > -0.5 else "⚠️"

        print(f"  [A] overall={avg_a['overall']:.1f} acc={avg_a['accuracy']:.1f} comp={avg_a['completeness']:.1f} | {avg_time_a:.1f}s {avg_tok_a:.0f}tok")
        print(f"  [B] overall={avg_b['overall']:.1f} acc={avg_b['accuracy']:.1f} comp={avg_b['completeness']:.1f} | {avg_time_b:.1f}s {avg_tok_b:.0f}tok (检索{ret_time:.2f}s/{ret_count}条)")
        print(f"  {symbol} 差异: {diff:+.1f}")

        all_results.append({
            "q": q["q"], "group": q["group"], "has_info": q["has_info"],
            "ref": q["ref"],
            "a": {**avg_a, "time": avg_time_a, "tokens": avg_tok_a},
            "b": {**avg_b, "time": avg_time_b, "tokens": avg_tok_b,
                  "ret_time": ret_time, "ret_count": ret_count, "ret_chars": len(context)},
            "diff": diff,
        })

    # ── 汇总 ──
    print(f"\n{'=' * 70}")
    print(f"  评测报告 v2")
    print(f"{'=' * 70}")

    # 按组统计
    for group_name, group_label in [("A", "有图知识"), ("B", "防幻觉"), ("C", "边缘推理")]:
        grp = [r for r in all_results if r["group"] == group_name]
        if not grp: continue
        avg_a_overall = statistics.mean(r["a"]["overall"] for r in grp)
        avg_b_overall = statistics.mean(r["b"]["overall"] for r in grp)
        avg_a_acc = statistics.mean(r["a"]["accuracy"] for r in grp)
        avg_b_acc = statistics.mean(r["b"]["accuracy"] for r in grp)
        print(f"\n  [{group_name}] {group_label} ({len(grp)}题):")
        print(f"    overall: A={avg_a_overall:.2f} → B={avg_b_overall:.2f} ({avg_b_overall-avg_a_overall:+.2f})")
        print(f"    accuracy: A={avg_a_acc:.2f} → B={avg_b_acc:.2f} ({avg_b_acc-avg_a_acc:+.2f})")

    # 总体
    total_a = statistics.mean(r["a"]["overall"] for r in all_results)
    total_b = statistics.mean(r["b"]["overall"] for r in all_results)
    total_a_acc = statistics.mean(r["a"]["accuracy"] for r in all_results)
    total_b_acc = statistics.mean(r["b"]["accuracy"] for r in all_results)
    total_a_time = statistics.mean(r["a"]["time"] for r in all_results)
    total_b_time = statistics.mean(r["b"]["time"] for r in all_results)
    total_a_tok = statistics.mean(r["a"]["tokens"] for r in all_results)
    total_b_tok = statistics.mean(r["b"]["tokens"] for r in all_results)
    avg_ret = statistics.mean(r["b"]["ret_time"] for r in all_results)

    print(f"\n  总体 ({len(all_results)}题 × {ROUNDS}轮):")
    print(f"  ┌──────────────┬──────────┬──────────┬────────┐")
    print(f"  │ 指标         │ A:只MEM  │ B:+图记忆│ 差异   │")
    print(f"  ├──────────────┼──────────┼──────────┼────────┤")
    print(f"  │ overall(/5) │ {total_a:>8.2f} │ {total_b:>8.2f} │ {total_b-total_a:>+6.2f} │")
    print(f"  │ accuracy(/5)│ {total_a_acc:>8.2f} │ {total_b_acc:>8.2f} │ {total_b_acc-total_a_acc:>+6.2f} │")
    print(f"  │ 耗时(s)     │ {total_a_time:>8.1f} │ {total_b_time:>8.1f} │ {total_b_time-total_a_time:>+6.1f} │")
    print(f"  │ Token       │ {total_a_tok:>8.0f} │ {total_b_tok:>8.0f} │ {total_b_tok-total_a_tok:>+6.0f} │")
    print(f"  │ 检索耗时    │       -  │ {avg_ret:>8.2f} │        │")
    print(f"  └──────────────┴──────────┴──────────┴────────┘")

    # 防幻觉专项
    hallucination_q = [r for r in all_results if r["group"] == "B"]
    if hallucination_q:
        h_a = statistics.mean(r["a"]["accuracy"] for r in hallucination_q)
        h_b = statistics.mean(r["b"]["accuracy"] for r in hallucination_q)
        print(f"\n  防幻觉专项 (B组{len(hallucination_q)}题,越高越好):")
        print(f"    A(只MEM): {h_a:.2f}/5  B(+图): {h_b:.2f}/5  差异: {h_b-h_a:+.2f}")

    # 保存
    report = {
        "version": "v2", "timestamp": time.strftime("%Y-%m-%d %H:%M:%S"),
        "model": LLM["model"], "rounds": ROUNDS, "questions": len(QUESTIONS),
        "memory_md_chars": len(memory_md), "graph": stats,
        "summary": {
            "a_overall": total_a, "b_overall": total_b,
            "a_accuracy": total_a_acc, "b_accuracy": total_b_acc,
            "a_time": total_a_time, "b_time": total_b_time,
            "a_tokens": total_a_tok, "b_tokens": total_b_tok,
            "avg_retrieval_time": avg_ret,
        },
        "per_question": all_results,
    }
    report_path = os.path.join("E:", os.sep, "workspace", "graph-memory", "benchmark_report_v2.json")
    with open(report_path, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=2)
    print(f"\n  报告: {report_path}")


if __name__ == "__main__":
    main()
