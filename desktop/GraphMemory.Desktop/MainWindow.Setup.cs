using Avalonia.Controls;
using Avalonia.Interactivity;
using System.Diagnostics;
using System.Text.Json.Nodes;

namespace GraphMemory.Desktop;
public partial class MainWindow {
    SetupPlan? setupPlan;
    bool setupInitializing;
    void SetupTargetChanged(object? sender, SelectionChangedEventArgs e) {
        if(setupInitializing || this.FindControl<TextBox>("SetupPath")==null)return;
        if(Find<ComboBox>("SetupTarget").SelectedItem is AgentTarget target)Set("SetupPath",target.Path);
        InvalidateSetup();
    }
    void SetupPathChanged(object? sender, TextChangedEventArgs e) { if(this.FindControl<TextBlock>("SetupStatus")!=null)InvalidateSetup(); }
    void InvalidateSetup() {
        setupPlan=null;Find<Button>("InstallAgentButton").IsEnabled=false;Find<Button>("UndoSetupButton").IsEnabled=false;
        Find<TextBlock>("SetupStatus").Text="点击检测配置，确认接入位置。";
    }
    async void AgentSetupClick(object? s,RoutedEventArgs e)=>await Run(async()=>{
        ShowEditor(false);Find<Grid>("HomePanel").IsVisible=false;Find<Border>("SetupPanel").IsVisible=true;
        if(Find<ComboBox>("SetupTarget").ItemsSource==null) {
            setupInitializing=true;Find<ComboBox>("SetupTarget").ItemsSource=AgentSetupService.Targets();Find<ComboBox>("SetupTarget").SelectedIndex=0;setupInitializing=false;
            Set("SetupPath",((AgentTarget)Find<ComboBox>("SetupTarget").SelectedItem!).Path);
        }
        await InspectSetup();
    });
    void CloseSetupClick(object? s,RoutedEventArgs e){if(busy)return;Find<Border>("SetupPanel").IsVisible=false;Find<Grid>("HomePanel").IsVisible=true;}
    async Task InspectSetup() {
        setupPlan=null;
        Find<Button>("InstallAgentButton").IsEnabled=false;Find<Button>("UndoSetupButton").IsEnabled=false;
        var target=Find<ComboBox>("SetupTarget").SelectedItem as AgentTarget ?? throw new InvalidOperationException("请选择 Agent。");
        var path=Text("SetupPath");
        var info=await Api().GetAsync("api/service");
        if(!Equals(Find<ComboBox>("SetupTarget").SelectedItem,target)||Text("SetupPath")!=path)return;
        var plan=AgentSetupService.Inspect(target with {Path=path},Path.Combine(AppContext.BaseDirectory,"mcp_server.exe"),Path.Combine(AppContext.BaseDirectory,"graph-memory-service.exe"),info["data_dir"]!.GetValue<string>());
        setupPlan=plan;
        Find<TextBlock>("SetupStatus").Text=!plan.Changed?"配置已就绪。宿主是否实际调用，请查看首页调用记录。":$"{(plan.Before==null?"将创建配置":"已找到配置，写入前自动备份")}。{(plan.Replaced.Length>0?$"将替换 {plan.Replaced.Length} 个旧记忆入口。":"将配置 Graph Memory v1。")}";
        Find<Button>("InstallAgentButton").IsEnabled=plan.Changed;
        Find<Button>("UndoSetupButton").IsEnabled=AgentSetupService.LastReceipt(plan.Path)!=null;
    }
    async void DetectSetupClick(object? s,RoutedEventArgs e)=>await Run(InspectSetup);
    async void InstallAgentClick(object? s,RoutedEventArgs e)=>await Run(async()=>{
        if(setupPlan==null)throw new InvalidOperationException("请先检测配置。");
        var receipt=AgentSetupService.Apply(setupPlan);
        await InspectSetup();
        Find<TextBlock>("SetupStatus").Text=receipt==null?"配置已就绪，无需重复写入。":$"已接入，{(receipt.Existed?"原配置已备份":"可撤销本次创建")}。请在宿主中重载 MCP 或重启宿主。";
    });
    async void UndoSetupClick(object? s,RoutedEventArgs e)=>await Run(async()=>{
        var receipt=AgentSetupService.LastReceipt(Text("SetupPath")) ?? throw new InvalidOperationException("没有可安全恢复的配置；接入后可能已被其他程序修改。");
        AgentSetupService.Restore(receipt);await InspectSetup();Find<TextBlock>("SetupStatus").Text="已恢复接入前的配置。请重载宿主的 MCP 配置。";
    });
    async void VerifyAgentClick(object? s,RoutedEventArgs e)=>await Run(async()=>{
        Find<TextBlock>("SetupStatus").Text="正在测试本地 MCP 组件…";
        var info=await Api().GetAsync("api/service");
        var utf8=new System.Text.UTF8Encoding(false);
        var start=new ProcessStartInfo(Path.Combine(AppContext.BaseDirectory,"mcp_server.exe")){UseShellExecute=false,CreateNoWindow=true,RedirectStandardInput=true,RedirectStandardOutput=true,RedirectStandardError=true,StandardInputEncoding=utf8,StandardOutputEncoding=utf8,StandardErrorEncoding=utf8};
        foreach(var key in start.Environment.Keys.Where(k=>k.StartsWith("GM_",StringComparison.Ordinal)).ToArray())start.Environment.Remove(key);
        start.Environment["GM_MEMORY_V1"]="1";start.Environment["GM_SERVICE_DIR"]=info["data_dir"]!.GetValue<string>();start.Environment["GM_SERVICE_EXE"]=Path.Combine(AppContext.BaseDirectory,"graph-memory-service.exe");
        using var process=Process.Start(start)??throw new InvalidOperationException("无法启动 MCP 组件。");
        var output=process.StandardOutput.ReadToEndAsync();var errors=process.StandardError.ReadToEndAsync();
        try {
            await process.StandardInput.WriteLineAsync("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}");
            await process.StandardInput.WriteLineAsync("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}");process.StandardInput.Close();
            await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(15));
            var lines=(await output).Split('\n',StringSplitOptions.RemoveEmptyEntries).Select(x=>JsonNode.Parse(x)!).ToList();
            var list=lines.Single(x=>x["id"]?.GetValue<int>()==2)["result"]?["tools"]?.AsArray();
            var expected=new HashSet<string>{"memory_projects","memory_search","memory_context","memory_write","memory_update"};
            if(process.ExitCode!=0||list==null||!expected.SetEquals(list.Select(x=>x!["name"]!.GetValue<string>())))throw new InvalidOperationException("MCP 工具列表异常，请检查发布组件。");
            Find<TextBlock>("SetupStatus").Text="本地组件测试通过，提供 5 个记忆工具。这不代表所选宿主已加载；请在宿主中开始新会话并查看调用记录。";
        } catch {Find<TextBlock>("SetupStatus").Text="本地组件测试失败，请检查组件与服务后重试。";throw;}
        finally {if(!process.HasExited)process.Kill();await errors;}
    });
}
