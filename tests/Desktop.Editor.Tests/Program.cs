using Avalonia;
using Avalonia.Controls;
using Avalonia.Headless;
using Avalonia.Threading;
using GraphMemory.Desktop;
using System.Reflection;
using System.Text.Json.Nodes;

var binary=Path.GetFullPath(args.Length>0?args[0]:"src-tauri/target/debug/graph-memory-service.exe");
var directory=Path.Combine(Path.GetTempPath(),"gm-editor-"+Guid.NewGuid().ToString("N"));
var host=new ServiceHost();
MainWindow? window=null;
int checks=0;
try {
    using var client=new ServiceClient(host.EnsureAsync(["--data-dir",directory,"--service-exe",binary]).GetAwaiter().GetResult());
    AppBuilder.Configure<App>().UseHeadless(new AvaloniaHeadlessPlatformOptions()).SetupWithoutStarting();
    window=new MainWindow();
    const BindingFlags flags=BindingFlags.Instance|BindingFlags.NonPublic;
    typeof(MainWindow).GetField("client",flags)!.SetValue(window,client);
    object? Invoke(string name,params object?[] parameters)=>typeof(MainWindow).GetMethod(name,flags)!.Invoke(window,parameters);
    T Control<T>(string name) where T:Control=>window.FindControl<T>(name)!;
    void Check(bool condition,string message){if(!condition)throw new Exception(message);checks++;}
    void Pump(Func<bool> done){var deadline=DateTime.UtcNow.AddSeconds(15);while(!done()){Dispatcher.UIThread.RunJobs();if(DateTime.UtcNow>deadline)throw new TimeoutException("UI operation timed out");Thread.Sleep(5);}Dispatcher.UIThread.RunJobs();}
    void Click(string method){Invoke(method,null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);}
    void Complete(Task task){Pump(()=>task.IsCompleted);task.GetAwaiter().GetResult();}
    JsonNode Post(string path,object body)=>client.PostAsync(path,body).GetAwaiter().GetResult();
    JsonNode Read(string id)=>Post("api/v1/search",new{scope="all"})["memories"]!.AsArray().Single(x=>x!["id"]!.GetValue<string>()==id)!.DeepClone();
    var projectId=Post("api/v1/projects",new{name="Editor regression"})["id"]!.GetValue<string>();
    foreach(var kind in new[]{"lesson","自定义经验","","decision"}) {
        Invoke("ClearEditor");
        var memory=Post("api/v1/memories",new{memory_scope="personal",kind,event_id=Guid.NewGuid().ToString(),title="Kind test",content="original"})["memory"]!;
        var id=memory["id"]!.GetValue<string>();
        Invoke("ApplyMemory",new MemoryItem(memory.DeepClone()));
        Check(!(bool)Invoke("HasDraft")!,"opening kind creates false draft: "+kind);
        Click("CloseEditorClick");
        Check(Control<Button>("NewMemoryButton").Content?.ToString()=="记一条","closing untouched kind retains draft");
        Invoke("ApplyMemory",new MemoryItem(Read(id)));
        Control<TextBox>("ContentInput").Text="edited";
        Check((bool)Invoke("HasDraft")!,"content edit not detected");
        Complete((Task)Invoke("SwitchScope",new ProjectItem(projectId,"Editor regression"))!);
        Complete((Task)Invoke("SwitchScope",new object?[]{null})!);
        Check(Control<TextBox>("ContentInput").Text=="edited","draft content lost across scopes");
        Click("SaveClick");
        var saved=Read(id);
        Check(saved["kind"]!.GetValue<string>()==kind,"content save changed kind: "+kind);
        Check(saved["content"]!.GetValue<string>()=="edited","content was not saved");
        Invoke("ApplyMemory",new MemoryItem(saved));
        Control<ComboBox>("Kind").SelectedIndex=0;
        Control<TextBox>("ContentInput").Text="explicit note";
        Complete((Task)Invoke("SwitchScope",new ProjectItem(projectId,"Editor regression"))!);
        Complete((Task)Invoke("SwitchScope",new object?[]{null})!);
        Click("SaveClick");
        Check(Read(id)["kind"]!.GetValue<string>()=="note","explicit note selection lost across draft restore");
        Invoke("ApplyMemory",new MemoryItem(Read(id)));
        Click("UndoClick");
        Check(Read(id)["kind"]!.GetValue<string>()==kind,"undo did not restore original kind");
        Check(!(bool)Invoke("HasDraft")!,"undo generated false draft");
        Check(Control<ComboBox>("Kind").ItemCount<=6,"custom options accumulate");
    }
    Invoke("ClearEditor");
    Control<TextBox>("ContentInput").Text="new default kind";
    Click("SaveClick");
    var created=Post("api/v1/search",new{query="new default kind"})["memories"]!.AsArray().Single()!;
    Check(created["kind"]!.GetValue<string>()=="note","new default kind is not note");
    Console.WriteLine($"PASS {checks} editor checks using real XAML controls + isolated HTTP service");
} finally {
    window?.Close();host.StopOwned();
}
