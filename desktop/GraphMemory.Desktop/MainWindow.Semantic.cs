using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Threading;
using System.Collections.ObjectModel;
using System.Text.Json.Nodes;

namespace GraphMemory.Desktop;

// P5-C 语义检索体验（契约 docs/design/P5_SEMANTIC_CONTRACT.md §2.2 / §3.3 / §5）：
// 可发现入口 + 状态（未启用/下载中/校验中/已就绪/失败/未知）+ 一键获取 + 进度可见 + 失败重试 +
// 默认关闭且可持久化的开关 + 未就绪时一切照旧 + 语义结果与字面结果视觉可区分。
// 全部网络访问都复用既有 ServiceClient（loopback、10 秒超时）；不新增网络能力，也不静默联网。
public partial class MainWindow {
    readonly DispatcherTimer semanticTimer = new() { Interval = TimeSpan.FromSeconds(5) };
    readonly ObservableCollection<SemanticItem> semanticItems = new();
    SemanticSettings settings = SemanticSettings.Default;
    SemanticModelStatus semanticStatus = SemanticModelStatus.NotConnected;
    string semanticNotice = "";
    bool semanticLoaded, semanticPolling;

    // 测试接缝：把“未就绪 / 接口缺失 / 失败”变成可复现的默认路径；生产代码永远走真实服务。
    ISemanticService? semanticOverride;
    internal void UseSemanticServiceForTests(ISemanticService? service) => semanticOverride = service;
    internal SemanticModelStatus SemanticModelState => semanticStatus;
    internal string SemanticNoticeText => semanticNotice;
    internal SemanticSettings SemanticSettingsSnapshot => settings;
    internal bool SemanticSearchEnabled => settings.SearchEnabled;

    ISemanticService SemanticClient() => semanticOverride
        ?? (client == null
            ? throw new InvalidOperationException("服务尚未连接，请点击“重试连接”。")
            : new ServiceSemanticClient(client));

    // ctor 里调用：XAML 已加载，控件可用；恢复持久化设置并挂上轮询。
    void InitializeSemantic() {
        settings = SemanticSettings.Load(out var note);
        semanticLoaded = true;
        if (note != null) semanticNotice = note;   // 设置文件损坏/为空时给出可读说明，绝不静默改行为
        var list = Find<ListBox>("SemanticMemories");
        if (list != null) list.ItemsSource = semanticItems;
        semanticTimer.Tick += async (_, _) => await PollSemanticStatusAsync();
        RenderSemanticPanel();
    }

    // —— 开关：默认关；只有模型已就绪才允许打开（未就绪时置灰并说明原因，不给“以为已经能搜到”的错觉） ——
    void SemanticToggleClick(object? s, RoutedEventArgs e) {
        if (busy) return;
        var toggle = Find<CheckBox>("SemanticToggle");
        if (toggle == null) return;
        if (toggle.IsChecked == true && !semanticStatus.IsReady) {
            toggle.IsChecked = false;
            SetSemanticNotice($"模型还没有就绪（当前：{SemanticStateLabel}），语义检索不能打开。请先点“获取模型”，就绪后再打开；不打开时检索行为与 0.3.0 完全一致。");
            return;
        }
        settings = new SemanticSettings(toggle.IsChecked == true);
        var error = SemanticSettings.Save(settings);
        SetSemanticNotice(error ?? (settings.SearchEnabled
            ? "语义检索已打开：搜索会在字面结果之外补充“可能相关（语义）”分组；该分组按意思匹配，可信度低于原话命中。"
            : "语义检索已关闭：检索行为与 0.3.0 完全一致（只按字面匹配）。"));
        RenderSemanticSearchHint();
    }

    // —— 状态：`GET /api/v1/agent/status` 的 semantic 字段；字段缺失（旧服务）→ 未知，不当作失败 ——
    async Task RefreshSemanticStatusAsync() {
        if (client == null && semanticOverride == null) { ApplySemanticStatus(SemanticModelStatus.NotConnected); return; }
        try {
            var status = await SemanticClient().AgentStatusAsync();
            semanticStatus = SemanticModelStatus.FromAgentStatus(status);
            semanticNotice = "";
        } catch (Exception e) {
            // 状态查询失败绝不能影响其它功能：保留字面检索与列表，只把原因写进可见说明。
            semanticStatus = new SemanticModelStatus(SemanticModelStatus.Unknown, $"无法读取模型状态（{FailureNote(e)}）。其余功能不受影响。", null, null, null);
            semanticNotice = semanticStatus.Detail ?? "";
        }
        ApplySemanticStatus(semanticStatus);
    }

    async Task PollSemanticStatusAsync() {
        await RefreshSemanticStatusAsync();
        if (!semanticStatus.IsBusy) StopSemanticPolling();
    }

    void StartSemanticPolling() {
        if (semanticPolling) return;
        semanticPolling = true;
        semanticTimer.Interval = TimeSpan.FromSeconds(1);
        semanticTimer.Start();
    }

    void StopSemanticPolling() {
        semanticPolling = false;
        semanticTimer.Stop();
        semanticTimer.Interval = TimeSpan.FromSeconds(5);
    }

    void ApplySemanticStatus(SemanticModelStatus status) {
        semanticStatus = status;
        RenderSemanticPanel();
        var toggle = Find<CheckBox>("SemanticToggle");
        if (toggle != null) toggle.IsEnabled = status.IsReady;   // 未就绪一律置灰
    }

    string SemanticStateLabel => semanticStatus.State switch {
        SemanticModelStatus.Ready => "已就绪",
        SemanticModelStatus.Downloading => "下载中",
        SemanticModelStatus.Verifying => "校验中",
        SemanticModelStatus.Failed => "失败",
        SemanticModelStatus.Absent => "未启用",
        _ => "未知",
    };

    static string FormatBytes(long bytes) => bytes >= 1024L * 1024L
        ? $"{bytes / (1024d * 1024d):0.0} MB"
        : $"{bytes / 1024d:0.0} KB";

    // 进度只来自结构化字段（progress / bytes_downloaded / bytes_total），缺字段时显示“不确定进度”。
    string SemanticProgressText() {
        var status = semanticStatus;
        if (status.HasProgress) return $" {status.Progress!.Value * 100:0}%";
        if (status.HasBytes) return $" {FormatBytes(status.BytesDownloaded!.Value)} / {FormatBytes(status.BytesTotal!.Value)}";
        return status.IsBusy ? "（具体进度未知）" : "";
    }

    string SemanticReasonText() => semanticStatus.State switch {
        SemanticModelStatus.Absent => "开启后换种说法也能搜到自己的记录。模型约 100~150 MB，只下载一次，之后完全离线；不点“获取模型”就不会联网。",
        SemanticModelStatus.Downloading => "正在把模型下载到本机。下载期间搜索仍按字面进行，可以继续正常使用。",
        SemanticModelStatus.Verifying => "下载完成，正在校验文件完整性；校验不通过会按“没有模型”降级，不会半途可用。",
        SemanticModelStatus.Ready => "模型已就绪。打开上方“语义检索”后，搜索会在字面结果之外补充“可能相关（语义）”分组；该分组按意思匹配，可信度低于原话命中。",
        SemanticModelStatus.Failed => "获取模型失败，检索仍按字面进行，其它功能不受影响。可点“重试获取”；反复失败请检查本机网络后重试。",
        _ => "当前服务没有提供语义模型状态（可能是 0.4.0 之前的版本）。检索按字面进行，行为与 0.3.0 一致；语义模型约 100~150 MB，只下载一次，之后完全离线。升级本地服务后这里会出现获取入口。",
    };

    void RenderSemanticPanel() {
        if (!semanticLoaded) return;
        var state = Find<TextBlock>("SemanticState");
        if (state != null) state.Text = $"语义检索：{SemanticStateLabel}{SemanticProgressText()}";
        // 侧边栏常驻一行，保证入口“可发现”：不打开面板也能看到当前状态。
        var sidebar = Find<TextBlock>("SemanticStatus");
        if (sidebar != null) sidebar.Text = $"语义检索：{SemanticStateLabel}{SemanticProgressText()}"
            + (settings.SearchEnabled ? "（已打开）" : "（未打开）");
        var reason = Find<TextBlock>("SemanticReason");
        if (reason != null) reason.Text = SemanticReasonText();
        var bar = Find<ProgressBar>("SemanticProgress");
        if (bar != null) {
            bar.IsVisible = semanticStatus.IsBusy;
            if (semanticStatus.HasProgress) { bar.IsIndeterminate = false; bar.Value = semanticStatus.Progress!.Value; }
            else if (semanticStatus.IsBusy) { bar.IsIndeterminate = true; bar.Value = 0; }
            else { bar.IsIndeterminate = false; bar.Value = 0; }
        }
        var prepare = Find<Button>("SemanticPrepareButton");
        if (prepare != null) {
            prepare.IsVisible = semanticStatus.IsAbsent || semanticStatus.IsFailed;
            prepare.IsEnabled = !semanticStatus.IsBusy;
            prepare.Content = semanticStatus.IsFailed ? "重试获取" : "获取模型（约 100~150 MB）";
        }
        var retry = Find<Button>("SemanticRetryButton");
        if (retry != null) retry.IsVisible = semanticStatus.IsFailed;
        SetSemanticNotice(semanticNotice);
        RenderSemanticSearchHint();
        var toggle = Find<CheckBox>("SemanticToggle");
        if (toggle != null) { toggle.IsChecked = settings.SearchEnabled; toggle.IsEnabled = semanticStatus.IsReady; }
    }

    void RenderSemanticSearchHint() {
        var hint = Find<TextBlock>("SemanticSearchHint");
        if (hint == null) return;
        hint.Text = settings.SearchEnabled
            ? (semanticStatus.IsReady
                ? "语义检索已打开：字面结果与“可能相关（语义）”分组同屏展示，两者不会混在同一个排序里。"
                : "已保存的设置为“打开”，但模型当前未就绪；本次检索按字面进行，与服务端的降级行为一致。")
            : "语义检索关闭时，检索行为与 0.3.0 完全一致（请求不带 semantic 字段）。";
    }

    void SetSemanticNotice(string text) {
        semanticNotice = text;
        var label = Find<TextBlock>("SemanticNotice");
        if (label == null) return;
        label.IsVisible = text != "";
        label.Text = text;
    }

    // 设置文件读取/写入失败也必须让用户看见（例如目录只读），不能静默把开关状态丢掉。
    internal void SetSemanticNoticeForTests(string text) => SetSemanticNotice(text);

    // —— 一键获取：`POST /api/v1/models/prepare`（本机、无参数、幂等），随后轮询状态显示阶段与进度 ——
    async void SemanticPrepareClick(object? s, RoutedEventArgs e) => await Run(async() => {
        SetSemanticNotice("正在请求本地服务准备模型…");
        try {
            var response = await SemanticClient().PrepareModelAsync();
            var detail = ReadJsonString(response?["detail"]);
            SetSemanticNotice(detail ?? "已请求本地服务准备模型，接下来会显示下载与校验进度。");
        } catch (Exception e) {
            var message = FailureNote(e) ?? "未知错误";
            // 接口不存在（旧服务）与真实失败要区分开：前者是“服务版本不支持”，后者可以重试。
            if (message.Contains("HTTP 404") || message.Contains("HTTP 405")) {
                semanticStatus = new SemanticModelStatus(SemanticModelStatus.Unknown, "当前本地服务没有提供“获取模型”接口（HTTP 404），无法下载模型。请升级本地服务；检索仍按字面进行。", null, null, null);
                SetSemanticNotice(semanticStatus.Detail!);
            } else {
                semanticStatus = new SemanticModelStatus(SemanticModelStatus.Failed, $"获取模型失败：{message}", null, null, null);
                SetSemanticNotice($"获取模型失败：{message} 检索仍按字面进行，其它功能不受影响；可点“重试获取”。");
            }
            ApplySemanticStatus(semanticStatus);
            return;
        }
        await RefreshSemanticStatusAsync();
        if (semanticStatus.IsBusy) StartSemanticPolling();
    });

    async void SemanticRefreshClick(object? s, RoutedEventArgs e) => await Run(async() => {
        await RefreshSemanticStatusAsync();
        if (semanticStatus.IsBusy) StartSemanticPolling();
        SetSemanticNotice(semanticNotice == "" ? "已刷新模型状态。" : semanticNotice);
    });

    // —— 检索请求体：唯一的构造入口。语义关闭时请求里没有 semantic 字段（与 0.3.0 逐字节一致） ——
    public static JsonObject BuildSearchBody(string? projectId, string query, bool includeRetracted, bool semantic) {
        var body = new JsonObject {
            ["project_id"] = projectId,
            ["query"] = query,
            ["include_retracted"] = includeRetracted,
        };
        if (semantic) body["semantic"] = true;
        return body;
    }

    JsonNode SearchAsync(string query) =>
        Api().PostAsync("api/v1/search", BuildSearchBody(project?.Id, query, ShowRetracted(), settings.SearchEnabled)).GetAwaiter().GetResult();

    // —— 语义分组：来自与字面结果同一次响应的 semantic 对象（不额外发请求，也不会混排序） ——
    internal void ApplySemanticFromResponse(JsonNode? response) {
        var semantic = response?["semantic"] as JsonObject;
        if (semantic == null) {
            HideSemanticGroup();
            if (settings.SearchEnabled)
                SetSemanticNotice("当前服务没有返回语义结果对象（可能是 0.4.0 之前的版本）。本次检索按字面进行，行为与关闭语义时一致。");
            return;
        }
        var available = ReadJsonBool(semantic["available"]);
        var mode = ReadJsonString(semantic["mode"]) ?? "literal";
        var note = ReadJsonString(semantic["note"]);
        var items = (semantic["memories"] as JsonArray ?? new JsonArray())
            .Select(node => new SemanticItem(node?.DeepClone() ?? new JsonObject(), ReadJsonDouble(node?["score"])))
            .Where(item => item.Id != "")
            .ToList();
        if (available && mode == "hybrid" && items.Count > 0) {
            ShowSemanticGroup(items, note);
            return;
        }
        HideSemanticGroup();
        if (settings.SearchEnabled)
            SetSemanticNotice(note ?? (available
                ? "本次没有语义候选：字面结果已经足够，语义通道没有补充内容。"
                : "模型未就绪，本次语义通道已跳过；字面结果与关闭语义时完全一致。"));
    }

    void ShowSemanticGroup(IReadOnlyList<SemanticItem> items, string? note) {
        semanticItems.Clear();
        foreach (var item in items) semanticItems.Add(item);
        var panel = Find<Border>("SemanticGroup");
        if (panel != null) panel.IsVisible = true;
        var hint = Find<TextBlock>("SemanticGroupHint");
        if (hint != null) hint.Text = note is { Length: > 0 }
            ? $"这些结果不是原话命中，是按意思匹配，可信度低于上面的字面结果。{note}"
            : "这些结果不是原话命中，是按意思匹配，可信度低于上面的字面结果；不会与字面结果混排。";
    }

    internal void HideSemanticGroup() {
        semanticItems.Clear();
        var panel = Find<Border>("SemanticGroup");
        if (panel != null) panel.IsVisible = false;
    }

    internal static string? ReadJsonString(JsonNode? node) { try { return node?.GetValue<string>(); } catch { return null; } }
    internal static bool ReadJsonBool(JsonNode? node) { try { return node?.GetValue<bool>() ?? false; } catch { return false; } }
    internal static double ReadJsonDouble(JsonNode? node) {
        try { return node == null || node.GetValueKind() != System.Text.Json.JsonValueKind.Number ? 0d : node.GetValue<double>(); }
        catch { return 0d; }
    }
}
