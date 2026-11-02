using Avalonia;
using Avalonia.Controls;
using Avalonia.Headless;
using Avalonia.Input;
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
    var url=host.EnsureAsync(["--data-dir",directory,"--service-exe",binary]).GetAwaiter().GetResult();
    using var client=new ServiceClient(url);
    typeof(MainWindow).Assembly.GetType("GraphMemory.Desktop.Program")!.GetField("Arguments",BindingFlags.Static|BindingFlags.Public)!.SetValue(null,new[]{"--service-url",url});
    AppBuilder.Configure<App>().UseHeadless(new AvaloniaHeadlessPlatformOptions()).SetupWithoutStarting();
    window=new MainWindow();
    const BindingFlags flags=BindingFlags.Instance|BindingFlags.NonPublic;
    typeof(MainWindow).GetField("client",flags)!.SetValue(window,new ServiceClient(url));
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
    window.Close();
    bool closed=false;
    void OpenWindow(){closed=false;window=new MainWindow();window.Closed+=(_,_)=>closed=true;window.Show();Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);}
    bool PromptVisible()=>Control<Border>("ClosePrompt").IsVisible;
    OpenWindow();window.Close();Check(closed,"clean window should close without prompt");
    OpenWindow();
    Control<TextBox>("TitleInput").Text="title-only draft";
    window.Close();Check(!closed,"dirty window closed without protection");Check(PromptVisible(),"close prompt missing");
    window.Close();Check(!closed&&PromptVisible(),"repeated close bypassed prompt");
    window.KeyPressQwerty(PhysicalKey.Escape,RawInputModifiers.None);window.KeyReleaseQwerty(PhysicalKey.Escape,RawInputModifiers.None);
    Check(!closed&&!PromptVisible(),"Escape should cancel close");
    Check(Control<TextBox>("TitleInput").Text=="title-only draft","cancel lost title-only draft");
    window.Close();Click("SaveAndCloseClick");
    Check(!closed,"invalid empty-content save closed window");
    Check(Control<TextBox>("TitleInput").Text=="title-only draft","invalid save lost draft");
    window.Close();Click("DiscardAndCloseClick");Check(closed,"explicit discard did not close");
    Check(Post("api/v1/search",new{query="title-only draft",scope="all"})["total"]!.GetValue<int>()==0,"discard wrote a record");

    OpenWindow();Control<TextBox>("ContentInput").Text="inactive-project-draft";
    Complete((Task)Invoke("SwitchScope",new ProjectItem(projectId,"Editor regression"))!);
    window.Close();Check(!closed&&PromptVisible(),"inactive-scope-only draft was ignored");
    Check(Control<TextBlock>("ClosePromptMessage").Text!.Contains("1"),"pending draft count wrong");
    Check(!Control<Grid>("MainContent").IsEnabled,"background UI not blocked by close prompt");
    Click("CancelCloseClick");
    Complete((Task)Invoke("SwitchScope",new object?[]{null})!);
    Check(Control<TextBox>("ContentInput").Text=="inactive-project-draft","inactive draft lost after cancel");
    window.Close();Click("DiscardAndCloseClick");Check(closed,"inactive draft discard did not close");

    OpenWindow();
    Control<TextBox>("ContentInput").Text="close-personal-success";Invoke("SetKind","lesson");
    Complete((Task)Invoke("SwitchScope",new ProjectItem(projectId,"Editor regression"))!);
    Control<TextBox>("ContentInput").Text="close-project-success";
    window.Close();Check(!closed&&PromptVisible(),"multiple-scope drafts not protected");
    Click("SaveAndCloseClick");Check(closed,"all-success save did not close");
    var closePersonal=Post("api/v1/search",new{query="close-personal-success",scope="all"})["memories"]!.AsArray();
    Check(closePersonal.Count==1&&closePersonal[0]!["memory_scope"]!.GetValue<string>()=="personal"&&closePersonal[0]!["kind"]!.GetValue<string>()=="lesson","personal draft scope/type incorrect");
    var closeProject=Post("api/v1/search",new{query="close-project-success",scope="all"})["memories"]!.AsArray();
    Check(closeProject.Count==1&&closeProject[0]!["project_id"]!.GetValue<string>()==projectId,"project draft saved into wrong scope");

    OpenWindow();
    Control<TextBox>("ContentInput").Text="partial-save-once";
    Complete((Task)Invoke("SwitchScope",new ProjectItem(projectId,"Editor regression"))!);
    var conflict=Post("api/v1/memories",new{memory_scope="project",project_id=projectId,event_id=Guid.NewGuid().ToString(),content="original conflict",kind="lesson"})["memory"]!;
    var conflictId=conflict["id"]!.GetValue<string>();
    Invoke("ApplyMemory",new MemoryItem(conflict.DeepClone()));Control<TextBox>("ContentInput").Text="keep-my-unsaved-correction";
    Post("api/v1/memories/update",new{id=conflictId,expected_version=1,content="concurrent verified correction"});
    window.Close();Click("SaveAndCloseClick");
    Check(!closed,"conflicting save closed window");
    Check(Control<TextBox>("ContentInput").Text=="keep-my-unsaved-correction","conflict erased unsaved draft");
    Check(Control<Button>("ReloadButton").IsVisible,"conflict recovery unavailable");
    Check(Post("api/v1/search",new{query="partial-save-once",scope="all"})["total"]!.GetValue<int>()==1,"first partial save missing/duplicated");
    window.Close();Click("SaveAndCloseClick");
    Check(!closed&&Control<TextBox>("ContentInput").Text=="keep-my-unsaved-correction","retry lost failed draft");
    Check(Post("api/v1/search",new{query="partial-save-once",scope="all"})["total"]!.GetValue<int>()==1,"retry duplicated successful draft");
    Check(Read(conflictId)["content"]!.GetValue<string>()=="concurrent verified correction","conflict overwrote newer record");
    Click("ReloadClick");window.Close();Check(closed,"resolved conflict still blocks close");

    OpenWindow();typeof(MainWindow).GetField("busy",flags)!.SetValue(window,true);
    window.Close();Check(!closed,"in-flight operation did not block closing");
    typeof(MainWindow).GetField("busy",flags)!.SetValue(window,false);window.Close();Check(closed,"completed clean operation still blocks close");

    OpenWindow();Control<TextBox>("ContentInput").Text="network-failure-draft";
    ((ServiceClient)typeof(MainWindow).GetField("client",flags)!.GetValue(window)!).Dispose();
    typeof(MainWindow).GetField("client",flags)!.SetValue(window,new ServiceClient("http://127.0.0.1:1"));
    window.Close();Click("SaveAndCloseClick");
    Check(!closed&&Control<TextBox>("ContentInput").Text=="network-failure-draft","network failure lost window/draft");
    window.Close();Click("CancelCloseClick");Check(!closed&&!PromptVisible(),"cancel after failure did not return to editing");
    window.Close();Click("DiscardAndCloseClick");Check(closed,"discard after network failure did not close");
    Console.WriteLine($"PASS {checks} editor checks using real XAML controls + isolated HTTP service");
} finally {
    window?.Close();host.StopOwned();
}
