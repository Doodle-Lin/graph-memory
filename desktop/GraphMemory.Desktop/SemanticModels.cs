using System.Text.Json;
using System.Text.Json.Nodes;

namespace GraphMemory.Desktop;

/// <summary>
/// P5-C：语义模型状态（`GET /api/v1/agent/status` 的 `semantic` 字段，契约 §3.3 / v1.1 进度字段）。
/// 解析必须防御式：字段缺失、类型异常、state 取值不在契约内时一律落到“状态未知”，绝不抛异常——
/// 旧版服务（0.3.0）根本没有这个字段，桌面端必须照旧可用。
/// </summary>
public sealed record SemanticModelStatus(
    string State,
    string? Detail,
    double? Progress,
    long? BytesDownloaded,
    long? BytesTotal) {
    public const string Absent = "absent";
    public const string Downloading = "downloading";
    public const string Verifying = "verifying";
    public const string Ready = "ready";
    public const string Failed = "failed";
    /// <summary>桌面端自造的状态：字段缺失或无法识别。它不是契约状态，只用于 UI 与测试判定。</summary>
    public const string Unknown = "unknown";

    public static readonly SemanticModelStatus NotConnected =
        new(Unknown, "服务未连接，暂时无法确认模型状态。", null, null, null);

    public bool IsReady => State == Ready;
    public bool IsBusy => State is Downloading or Verifying;
    public bool IsAbsent => State == Absent;
    public bool IsFailed => State == Failed;
    public bool HasProgress => Progress.HasValue;

    /// <summary>已下载字节数与总字节数（两者都有时才可用于显示）。</summary>
    public bool HasBytes => BytesDownloaded.HasValue && BytesTotal.HasValue && BytesTotal.Value > 0;

    public static SemanticModelStatus FromAgentStatus(JsonNode? agentStatus) {
        if (agentStatus?["semantic"] is not JsonObject node)
            return new(Unknown, "当前服务没有提供语义模型状态（可能是 0.4.0 之前的版本）。", null, null, null);
        var state = ReadString(node["state"])?.Trim().ToLowerInvariant() ?? "";
        if (state is not (Absent or Downloading or Verifying or Ready or Failed))
            return new(Unknown, $"服务返回了无法识别的语义状态：{(state == "" ? "(空)" : state)}", null, null, null);

        var progress = ReadDouble(node["progress"]);
        if (progress.HasValue) progress = Math.Clamp(progress.Value, 0d, 1d);
        var downloaded = ReadLong(node["bytes_downloaded"]);
        var total = ReadLong(node["bytes_total"]);
        // 契约 v1.1 第三条：ready / failed 之后进度字段必须消失或归零，客户端也不显示残留进度。
        if (state is Ready or Failed) { progress = null; downloaded = null; total = null; }
        return new(state, ReadString(node["detail"]), progress, downloaded, total);
    }

    static string? ReadString(JsonNode? node) {
        try { return node?.GetValue<string>(); } catch { return null; }
    }

    static double? ReadDouble(JsonNode? node) {
        try {
            if (node == null) return null;
            return node.GetValueKind() == JsonValueKind.Number ? node.GetValue<double>() : null;
        } catch { return null; }
    }

    static long? ReadLong(JsonNode? node) {
        try {
            if (node == null) return null;
            return node.GetValueKind() == JsonValueKind.Number ? node.GetValue<long>() : null;
        } catch { return null; }
    }
}

/// <summary>
/// P5-C：语义检索开关的持久化设置。
/// 沿用 P3-A 草稿的落盘位置与写法定式（`DraftStore.Root` + 临时文件改名替换），不新造存储体系；
/// 开关默认关闭，且读不出来时一律按“关闭”处理（关闭 = 与 0.3.0 完全一致）。
/// </summary>
public sealed record SemanticSettings(bool SearchEnabled) {
    public const int FormatVersion = 1;
    public const string FileName = "semantic-settings.json";

    public static readonly SemanticSettings Default = new(false);

    /// <summary>与草稿同目录：`%LOCALAPPDATA%\GraphMemory\desktop-drafts\semantic-settings.json`（`GM_DRAFT_DIR` 可覆盖）。</summary>
    public static string FilePath => Path.Combine(DraftStore.Root, FileName);

    /// <summary>读取设置；损坏、缺失或类型异常时返回默认值并给出可读说明（note 为 null 表示一切正常）。</summary>
    public static SemanticSettings Load(out string? note) {
        note = null;
        var file = FilePath;
        try {
            if (!File.Exists(file)) return Default;
            var text = File.ReadAllText(file, System.Text.Encoding.UTF8);
            if (string.IsNullOrWhiteSpace(text)) {
                note = $"语义检索设置文件为空，已按“关闭”处理；文件保留在 {file}，可自行删除。";
                return Default;
            }
            if (JsonNode.Parse(text) is not JsonObject root) {
                note = $"语义检索设置文件损坏（顶层不是 JSON 对象），已按“关闭”处理；文件保留在 {file}。";
                return Default;
            }
            var format = 0;
            try { format = root["format"]?.GetValue<int>() ?? -1; } catch { format = -1; }
            if (format != FormatVersion) {
                note = $"语义检索设置格式版本 {format} 不是预期的 {FormatVersion}，已按“关闭”处理。";
                return Default;
            }
            var enabled = false;
            try { enabled = root["search_enabled"]?.GetValue<bool>() ?? false; } catch {
                note = "语义检索设置的 search_enabled 不是布尔值，已按“关闭”处理。";
                return Default;
            }
            return new(enabled);
        } catch (Exception e) {
            note = $"语义检索设置读取失败（{e.Message}），已按“关闭”处理。";
            return Default;
        }
    }

    /// <summary>原子写入；返回 null 表示成功，否则返回可读失败原因（调用方必须显示，不得静默）。</summary>
    public static string? Save(SemanticSettings settings) {
        try {
            var file = FilePath;
            Directory.CreateDirectory(DraftStore.Root);
            var payload = new JsonObject {
                ["format"] = FormatVersion,
                ["search_enabled"] = settings.SearchEnabled,
                ["saved_at"] = DateTimeOffset.UtcNow.ToString("O"),
            };
            var temporary = $"{file}.{Guid.NewGuid():N}.tmp";
            File.WriteAllText(temporary, payload.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), new System.Text.UTF8Encoding(false));
            File.Move(temporary, file, true);
            return null;
        } catch (Exception e) {
            return $"语义检索设置保存失败：{e.Message}";
        }
    }
}

/// <summary>
/// P5-C：桌面端唯一的语义通道出入口。真实实现只走既有 `ServiceClient`（loopback、10 秒超时），
/// 不新增任何网络能力；测试可注入假实现，把“未就绪 / 接口缺失 / 失败”变成默认路径。
/// </summary>
public interface ISemanticService {
    Task<JsonNode> AgentStatusAsync();
    Task<JsonNode> PrepareModelAsync();
}

/// <summary>真实实现：`GET /api/v1/agent/status` 与 `POST /api/v1/models/prepare`（本机、无参数、幂等）。</summary>
public sealed class ServiceSemanticClient(ServiceClient client) : ISemanticService {
    public Task<JsonNode> AgentStatusAsync() => client.GetAsync("api/v1/agent/status");
    public Task<JsonNode> PrepareModelAsync() => client.PostAsync("api/v1/models/prepare", new { });
}

/// <summary>P5-C：语义结果列表项。分数与字面分数分开呈现，且明确写出“不是原话匹配”。</summary>
public sealed record SemanticItem(JsonNode Data, double Score) {
    public string Id { get { try { return Data["id"]?.GetValue<string>() ?? ""; } catch { return ""; } } }
    public string Title {
        get {
            try {
                var title = Data["title"]?.GetValue<string>() ?? "";
                if (!string.IsNullOrWhiteSpace(title)) return title;
                var content = Data["content"]?.GetValue<string>() ?? "";
                return content.Length > 40 ? content[..40] + "…" : content;
            } catch { return ""; }
        }
    }
    public string Preview {
        get {
            try {
                var text = (Data["content"]?.GetValue<string>() ?? "").Replace("\r", " ").Replace("\n", " ");
                return text.Length > 160 ? text[..160] + "…" : text;
            } catch { return ""; }
        }
    }
    public string ScoreLabel => $"语义相似度 {Score:0.00} · 按意思匹配，不是原话匹配";
    /// <summary>只有服务端附带了完整记录字段时，才允许在编辑器里打开它。</summary>
    public bool CanOpen {
        get {
            try {
                return Data["id"] != null && Data["title"] != null && Data["memory_scope"] != null
                    && Data["version"] != null && Data["content"] != null;
            } catch { return false; }
        }
    }
}
