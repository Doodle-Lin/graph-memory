using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace GraphMemory.Desktop;

/// <summary>
/// P3-A: 一条可持久化的桌面草稿。草稿只存在于用户目录下的草稿文件里，绝不进入记忆库。
/// </summary>
public sealed record DraftRecord(
    string Scope,
    string? MemoryId,
    ulong Version,
    string? Origin,
    string Kind,
    string Evidence,
    string Title,
    string Content,
    string Source,
    string Conditions,
    bool IsNew,
    string EventId,
    string Request);

/// <summary>读取结果：草稿集合 + 一句可读说明（文件缺失时说明为空）。</summary>
public sealed record DraftLoadResult(Dictionary<string,DraftRecord> Drafts,string? Note) {
    public static readonly DraftLoadResult Empty = new(new Dictionary<string,DraftRecord>(),null);
}

/// <summary>
/// P3-A 草稿仓库。
/// 位置：默认 %LOCALAPPDATA%\GraphMemory\desktop-drafts\，可用环境变量 GM_DRAFT_DIR 覆盖。
/// 分片：文件名 = 规范化 data_dir 的稳定哈希（SHA-256 前 16 位十六进制）；键含 scope（项目 id 或 personal）。
/// 写入：临时文件 + 改名替换，任意时刻中断只会丢最后一次保存，不会留下半截 JSON。
/// 读取：缺失 → 空集；非 JSON / 截断 / 字段类型不对 → 空集并给可读说明（文件保留，便于用户自查）。
/// 清理规则（确定性）：(1) 每次保存时删除 30 天未修改的分片；(2) 读取时若分片内部记录的 data_dir
/// 与其文件名哈希不一致（工作库已变化），该分片视为孤儿：不加载并删除。
/// 绝不写入 --data-dir 目录，避免污染备份/校验的输入集合。
/// </summary>
public static class DraftStore {
    public const int FormatVersion = 1;
    public const int RetentionDays = 30;
    public const string DefaultFolderName = "desktop-drafts";

    static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    /// <summary>草稿根目录；每次都读环境变量，便于测试与 CI 覆盖。</summary>
    public static string Root {
        get {
            var configured = Environment.GetEnvironmentVariable("GM_DRAFT_DIR");
            if (!string.IsNullOrWhiteSpace(configured)) return configured!;
            var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
            if (string.IsNullOrWhiteSpace(local)) local = Path.GetTempPath();
            return Path.Combine(local, "GraphMemory", DefaultFolderName);
        }
    }

    /// <summary>
    /// 规范化工作库目录：去掉 Windows 扩展前缀 \\?\、统一分隔符、去尾部分隔符、统一小写。
    /// 服务返回的 data_dir 可能带 \\?\ 前缀，而用户目录不带，所以必须先归一，否则同一工作库会分到两个桶。
    /// </summary>
    public static string Normalize(string? dataDir) {
        if (string.IsNullOrWhiteSpace(dataDir)) return "";
        var value = dataDir!.Trim();
        if (value.StartsWith(@"\\?\UNC\", StringComparison.Ordinal)) value = @"\\" + value[8..];
        else if (value.StartsWith(@"\\?\", StringComparison.Ordinal)) value = value[4..];
        value = value.Replace('/', Path.DirectorySeparatorChar);
        try { value = Path.GetFullPath(value); } catch { /* 无法规范化时按原样使用 */ }
        value = value.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        return value.ToLowerInvariant();
    }

    /// <summary>稳定哈希分片名：同一工作库永远得到同一个文件名。</summary>
    public static string BucketName(string? dataDir) {
        var normalized = Normalize(dataDir);
        if (normalized == "") return "gm-draft-no-workspace";
        var hash = SHA256.HashData(Encoding.UTF8.GetBytes(normalized));
        return Convert.ToHexString(hash)[..16].ToLowerInvariant();
    }

    /// <summary>该工作库对应的草稿文件（唯一写入口，且永远在 Root 之下，不在 data_dir 内）。</summary>
    public static string FileFor(string? dataDir) => Path.Combine(Root, BucketName(dataDir) + ".json");

    public static DraftLoadResult Load(string? dataDir) {
        var file = FileFor(dataDir);
        try {
            if (!File.Exists(file)) return DraftLoadResult.Empty;
            var text = File.ReadAllText(file, Encoding.UTF8);
            if (string.IsNullOrWhiteSpace(text)) return new(new Dictionary<string,DraftRecord>(), $"草稿文件为空，已按“没有草稿”处理：{file}");
            var root = JsonNode.Parse(text) as JsonObject;
            if (root == null) return Corrupt(file, "顶层不是 JSON 对象");
            var format = ReadInt(root["format"]);
            if (format != FormatVersion) return Corrupt(file, $"格式版本 {format} 不是预期的 {FormatVersion}");
            var stored = Normalize(ReadString(root["data_dir"]));
            var requested = Normalize(dataDir);
            if (stored != requested) {
                // 孤儿分片：文件名的哈希对不上内部记录的工作库（工作库已变化）。不加载，并按确定性规则清理。
                TryDelete(file);
                return new(new Dictionary<string,DraftRecord>(), $"发现属于其他工作库的草稿分片，已忽略并清理：{file}");
            }
            if (root["drafts"] is not JsonObject draftsNode) return Corrupt(file, "缺少 drafts 对象");
            var drafts = new Dictionary<string,DraftRecord>();
            foreach (var pair in draftsNode) {
                if (pair.Value is not JsonObject entry) return Corrupt(file, $"草稿 {pair.Key} 不是对象");
                var scope = ReadString(entry["scope"]);
                if (string.IsNullOrEmpty(scope) || scope != pair.Key) return Corrupt(file, $"草稿 {pair.Key} 的 scope 字段不匹配");
                var isNew = ReadBool(entry["is_new"]);
                var version = ReadVersion(entry["version"]);
                var memoryId = ReadOptionalString(entry["memory_id"]);
                drafts[scope] = new DraftRecord(
                    scope, memoryId, version, ReadOptionalString(entry["origin"]),
                    ReadString(entry["kind"]) ?? "", ReadString(entry["evidence_type"]) ?? "",
                    ReadString(entry["title"]) ?? "", ReadString(entry["content"]) ?? "",
                    ReadString(entry["source"]) ?? "", ReadString(entry["conditions"]) ?? "",
                    isNew, ReadString(entry["event_id"]) ?? "", ReadString(entry["request"]) ?? "");
            }
            return new(drafts, null);
        } catch (Exception e) {
            return Corrupt(file, e.Message);
        }
    }

    /// <summary>原子写入：先写临时文件，再改名替换目标文件。</summary>
    /// <remarks>data_dir 未知（服务未连接或没返回 data_dir）时拒绝写入并返回说明：
    /// 空工作库分片既违反“按 data_dir 隔离”，也会让草稿在重开后看起来丢失。</remarks>
    public static string? Save(string? dataDir, IEnumerable<DraftRecord> drafts) {
        if (Normalize(dataDir) == "") return "工作库目录未知（服务未连接或未返回 data_dir），草稿未写入。请先连接服务后重试。";
        try {
            var file = FileFor(dataDir);
            var payload = new JsonObject {
                ["format"] = FormatVersion,
                ["data_dir"] = Normalize(dataDir),
                ["saved_at"] = DateTimeOffset.UtcNow.ToString("O"),
                ["drafts"] = new JsonObject(drafts.Select(d => new KeyValuePair<string,JsonNode?>(d.Scope, ToNode(d)))),
            };
            Directory.CreateDirectory(Root);
            var text = payload.ToJsonString(WriteOptions);
            var temporary = $"{file}.{Guid.NewGuid():N}.tmp";
            File.WriteAllText(temporary, text, new UTF8Encoding(false));
            File.Move(temporary, file, true);
            PruneOrphans();
            return null;
        } catch (Exception e) {
            return $"草稿保存失败：{e.Message}";
        }
    }

    /// <summary>删除该工作库的整个草稿分片（放弃草稿 / 没有任何草稿时调用）。返回是否真的删除了文件。</summary>
    public static bool DeleteBucket(string? dataDir) {
        try {
            var file = FileFor(dataDir);
            if (!File.Exists(file)) return false;
            File.Delete(file);
            return true;
        } catch { return false; }
    }

    /// <summary>确定性清理：删除 30 天未修改的分片与内部 data_dir 对不上文件名的孤儿分片。返回删除数量。</summary>
    public static int PruneOrphans(int retentionDays = RetentionDays) {
        var removed = 0;
        try {
            if (!Directory.Exists(Root)) return 0;
            foreach (var file in Directory.EnumerateFiles(Root, "*.json")) {
                try {
                    var stale = File.GetLastWriteTimeUtc(file) < DateTime.UtcNow.AddDays(-retentionDays);
                    var orphan = false;
                    try {
                        var root = JsonNode.Parse(File.ReadAllText(file, Encoding.UTF8)) as JsonObject;
                        var stored = Normalize(root?["data_dir"]?.GetValue<string>());
                        orphan = stored != "" && Path.GetFileNameWithoutExtension(file) != BucketName(stored);
                    } catch { /* 损坏文件不作为孤儿删除，交给读取路径给说明 */ }
                    if (stale || orphan) { File.Delete(file); removed++; }
                } catch { /* 单个文件失败不阻断其他清理 */ }
            }
        } catch { /* 目录不可用时静默跳过；读取路径仍会给出说明 */ }
        return removed;
    }

    static JsonObject ToNode(DraftRecord draft) => new() {
        ["scope"] = draft.Scope,
        ["is_new"] = draft.IsNew,
        ["memory_id"] = draft.MemoryId,
        ["version"] = draft.Version,
        ["origin"] = draft.Origin,
        ["kind"] = draft.Kind,
        ["evidence_type"] = draft.Evidence,
        ["title"] = draft.Title,
        ["content"] = draft.Content,
        ["source"] = draft.Source,
        ["conditions"] = draft.Conditions,
        ["event_id"] = draft.EventId,
        ["request"] = draft.Request,
    };

    static DraftLoadResult Corrupt(string file, string reason) {
        // 视为空草稿集，但保留原文件并给出可读说明，绝不静默，也绝不因此崩溃。
        return new(new Dictionary<string,DraftRecord>(), $"草稿文件损坏（{reason}），已按“没有草稿”处理；原文件保留在 {file}，可自行删除。");
    }

    static void TryDelete(string file) { try { File.Delete(file); } catch { /* 删除失败不影响继续启动 */ } }
    static int ReadInt(JsonNode? node) { try { return node?.GetValue<int>() ?? -1; } catch { return -1; } }
    static bool ReadBool(JsonNode? node) { try { return node?.GetValue<bool>() ?? false; } catch { throw new InvalidOperationException("is_new 不是布尔值"); } }
    static ulong ReadVersion(JsonNode? node) { try { return node?.GetValue<ulong>() ?? 0; } catch { throw new InvalidOperationException("version 不是非负整数"); } }
    static string? ReadString(JsonNode? node) { try { return node?.GetValue<string>(); } catch { throw new InvalidOperationException("字段类型不是字符串"); } }
    static string? ReadOptionalString(JsonNode? node) {
        if (node == null) return null;
        if (node.GetValueKind() == System.Text.Json.JsonValueKind.Null) return null;
        try { return node.GetValue<string>(); } catch { throw new InvalidOperationException("可选字段类型不是字符串"); }
    }
}
