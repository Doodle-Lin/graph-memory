using Avalonia;
using Avalonia.Controls;
using Avalonia.Headless;
using Avalonia.Input;
using Avalonia.Threading;
using GraphMemory.Desktop;
using System.Reflection;
using System.Text.Json.Nodes;

// P3-A：同一个可执行文件既是套件，也是“进程 A 写草稿后被 Kill / 进程 B 读回”的探针端。
if(args.Length>0&&args[0]=="--draft-probe"){DraftProbe.Run(args);return;}

var binary=Path.GetFullPath(args.Length>0?args[0]:"src-tauri/target/debug/graph-memory-service.exe");
var directory=System.IO.Path.Combine(System.IO.Path.GetTempPath(),"gm-editor-"+Guid.NewGuid().ToString("N"));
Environment.SetEnvironmentVariable("GM_BACKUP_EXE",System.IO.Path.Combine(System.IO.Path.GetDirectoryName(binary) ?? ".","graph-memory-backup.exe"));
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

    // P1 软撤回 / 恢复：走既有控件与版本冲突流程，撤回不是删除。
    OpenWindow();
    var p1Id=Post("api/v1/memories",new{memory_scope="personal",event_id=Guid.NewGuid().ToString(),title="P1 retract",content="P1 retract body",kind="decision"})["memory"]!["id"]!.GetValue<string>();
    Complete((Task)Invoke("LoadMemories")!);
    Invoke("ApplyMemory",new MemoryItem(Post("api/v1/search",new{scope="all"})["memories"]!.AsArray().Single(x=>x!["id"]!.GetValue<string>()==p1Id)!.DeepClone()));
    Check(Control<Button>("RetractButton").IsVisible,"retract action missing on a live record");
    Check(!Control<Button>("RestoreButton").IsVisible,"restore action shown on a live record");
    Control<TextBox>("RetractReasonInput").Text="P1 用例撤回";Click("RetractClick");
    Check(Post("api/v1/search",new{query="P1 retract",scope="all"})["total"]!.GetValue<int>()==0,"retracted record still in the default search");
    Check(Control<ListBox>("Memories").ItemsSource!.Cast<MemoryItem>().All(x=>x.Id!=p1Id),"retracted record still in the default list");
    Check(Control<TextBlock>("RetractedBanner").IsVisible,"retracted banner missing");
    Check(Control<Button>("RestoreButton").IsVisible&&!Control<Button>("RetractButton").IsVisible,"editor actions not switched after retract");
    var p1Marked=Post("api/v1/search",new{query="P1 retract",scope="all",include_retracted=true})["memories"]!.AsArray().Single()!;
    Check(p1Marked["retracted_at"]!=null&&p1Marked["retracted_at"]!.GetValue<string>().Length>0,"retracted_at not written");
    Check(p1Marked["retract_reason"]!.GetValue<string>()=="P1 用例撤回","retract_reason not written");
    Check(p1Marked["version"]!.GetValue<int>()==2,"retract did not produce version 2");
    Control<CheckBox>("IncludeRetractedToggle").IsChecked=true;Click("IncludeRetractedClick");
    Check(Control<ListBox>("Memories").ItemsSource!.Cast<MemoryItem>().Single(x=>x.Id==p1Id).IsRetracted,"include_retracted list item not marked retracted");
    Click("RestoreClick");
    Check(Post("api/v1/search",new{query="P1 retract",scope="all"})["total"]!.GetValue<int>()==1,"restored record not searchable again");
    Check(Post("api/v1/search",new{scope="all",include_retracted=true})["memories"]!.AsArray().Single(x=>x!["id"]!.GetValue<string>()==p1Id)!["retracted_at"]==null,"restore did not clear retracted_at");
    Check(Post("api/v1/memories/history",new{id=p1Id})["history"]!.AsArray().Count==3,"retract/restore history must keep all 3 versions");

    // P2-A/B 数据管理面板：真实 graph-memory-backup.exe + 真实服务 + 临时目录。
    var dataTemp=System.IO.Path.Combine(System.IO.Path.GetTempPath(),"gm-data-"+Guid.NewGuid().ToString("N"));
    System.IO.Directory.CreateDirectory(dataTemp);
    var backupFile=System.IO.Path.Combine(dataTemp,"probe.gmbackup");
    var restoreTarget=System.IO.Path.Combine(dataTemp,"target");
    System.IO.Directory.CreateDirectory(restoreTarget);
    var truncatedFile=System.IO.Path.Combine(dataTemp,"truncated.gmbackup");
    var nonEmptyTarget=System.IO.Path.Combine(dataTemp,"nonempty");
    System.IO.Directory.CreateDirectory(nonEmptyTarget);
    var keptFile=System.IO.Path.Combine(nonEmptyTarget,"keep.txt");
    System.IO.File.WriteAllText(keptFile,"keep me");
    OpenWindow();
    Invoke("DataPanelClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Pump(()=>Control<TextBlock>("DataCountText").Text!.Contains("记录计数：当前视图"));
    Check(Control<Border>("DataPanel").IsVisible,"data panel did not open");
    Check(!Control<Grid>("HomePanel").IsVisible,"data panel did not replace the home panel");
    Check(Control<TextBlock>("DataDirText").Text!.Contains(System.IO.Path.GetFileName(directory.TrimEnd(System.IO.Path.DirectorySeparatorChar))),"data panel does not show the current workspace directory");
    Check(Control<TextBlock>("DataIntro").Text!.Contains("绝不覆盖当前工作库"),"data panel does not warn about never overwriting the workspace");
    Check(Control<Button>("ExportBackupButton").IsEnabled,"export disabled while connected");
    Check(Control<TextBlock>("DataCountText").Text!.Contains("记录计数"),"data panel does not report record counts");
    var beforeExport=Post("api/v1/search",new{scope="all",include_retracted=true})["total"]!.GetValue<int>();
    // 服务不可用/库目录未知时导出必须禁用并说明原因，且不调用备份程序。
    var liveClient=(ServiceClient)typeof(MainWindow).GetField("client",flags)!.GetValue(window)!;
    typeof(MainWindow).GetField("client",flags)!.SetValue(window,new ServiceClient("http://127.0.0.1:1"));
    typeof(MainWindow).GetField("dataDir",flags)!.SetValue(window,"");
    Invoke("RefreshDataPanel");
    Check(!Control<Button>("ExportBackupButton").IsEnabled,"export not disabled while the workspace directory is unknown");
    Check(Control<TextBlock>("DataDirText").Text!.Contains("未知")||Control<TextBlock>("DataDirText").Text!.Contains("未连接"),"disabled export does not explain why: "+Control<TextBlock>("DataDirText").Text);
    var neverFile=System.IO.Path.Combine(dataTemp,"never.gmbackup");
    Control<TextBox>("BackupPathInput").Text=neverFile;
    Invoke("ExportBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(!System.IO.File.Exists(neverFile),"export ran without a known workspace directory");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains("未调用备份程序"),"disabled export did not say the CLI was not called");
    typeof(MainWindow).GetField("client",flags)!.SetValue(window,liveClient);
    Invoke("DataPanelClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<Button>("ExportBackupButton").IsEnabled,"export not re-enabled after the service came back");
    Control<TextBox>("BackupPathInput").Text="";
    Invoke("ExportBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<TextBox>("DataOutput").Text!.Contains("请先填写导出目标文件路径"),"empty export path did not produce a visible message");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains("未执行"),"empty export path did not explain why nothing happened");
    Control<TextBox>("BackupPathInput").Text=backupFile;
    Invoke("ExportBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(System.IO.File.Exists(backupFile),"export did not create the backup file");
    Check(Control<TextBox>("DataOutput").Text!.Contains("\"status\":\"ok\""),"export did not show the CLI JSON");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains("导出成功"),"export success not reported: "+Control<TextBlock>("DataStatus2").Text);
    var exportInfo=new System.IO.FileInfo(backupFile);var exportSize=exportInfo.Length;var exportStamp=exportInfo.LastWriteTimeUtc;
    Invoke("VerifyBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<TextBox>("DataOutput").Text!.Contains("\"operation\":\"verify\""),"verify did not run through the UI");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains("校验通过"),"verify success not reported");
    Control<TextBox>("BackupPathInput").Text=backupFile;
    Invoke("ExportBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<TextBox>("DataOutput").Text!.Contains("目标文件已存在"),"existing target was not rejected before calling the CLI");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains("没有调用备份程序"),"existing target rejection not explained");
    var afterReject=new System.IO.FileInfo(backupFile);
    Check(afterReject.Length==exportSize&&afterReject.LastWriteTimeUtc==exportStamp,"existing backup was modified by a rejected export");
    Control<TextBox>("RestoreInputPath").Text=backupFile;
    Control<TextBox>("RestoreTargetDirInput").Text=restoreTarget;
    Invoke("RestoreDryRunClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<TextBox>("DataOutput").Text!.Contains("\"dry_run\":true"),"dry-run did not report dry_run");
    Check(System.IO.Directory.GetFileSystemEntries(restoreTarget).Length==0,"dry-run wrote files into the target directory");
    Invoke("RestoreApplyClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<Border>("RestoreConfirmPanel").IsVisible,"apply did not ask for a second confirmation");
    Check(Control<TextBlock>("RestoreConfirmText").Text!.Contains("空目录")&&Control<TextBlock>("RestoreConfirmText").Text!.Contains("不会被写入"),"confirmation does not state empty-target/no-overwrite terms");
    Check(System.IO.Directory.GetFileSystemEntries(restoreTarget).Length==0,"apply wrote files before confirmation");
    Invoke("ConfirmRestoreClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(System.IO.File.Exists(System.IO.Path.Combine(restoreTarget,"work-memory.db")),"restore did not create work-memory.db in the empty target");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains(restoreTarget),"restore did not report the target directory");
    Check(Control<TextBox>("DataOutput").Text!.Contains("\"status\":\"ok\""),"restore did not show the CLI JSON");
    var roundTripBackup=System.IO.Path.Combine(dataTemp,"roundtrip.gmbackup");
    var roundTrip=BackupClient.ExportAsync(restoreTarget,roundTripBackup).GetAwaiter().GetResult();
    Check(roundTrip.Ok,"export from the restored directory failed: "+roundTrip.Summary);
    var roundTripVerify=BackupClient.VerifyAsync(roundTripBackup).GetAwaiter().GetResult();
    Check(roundTripVerify.Ok&&roundTripVerify.Counts==roundTrip.Counts,"round-trip counts inconsistent: "+roundTripVerify.Summary);
    Control<TextBox>("RestoreInputPath").Text=backupFile;
    Control<TextBox>("RestoreTargetDirInput").Text=nonEmptyTarget;
    Invoke("RestoreApplyClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(!Control<Border>("RestoreConfirmPanel").IsVisible,"non-empty target reached the confirmation step");
    Check(Control<TextBox>("DataOutput").Text!.Contains("不是空目录"),"non-empty target rejection not shown: "+Control<TextBox>("DataOutput").Text);
    Check(System.IO.File.ReadAllText(keptFile)=="keep me","non-empty target was modified");
    Check(System.IO.Directory.GetFileSystemEntries(nonEmptyTarget).Length==1,"non-empty target gained files");
    var bytes=System.IO.File.ReadAllBytes(backupFile);
    System.IO.File.WriteAllBytes(truncatedFile,bytes.Take(bytes.Length/2).ToArray());
    Control<TextBox>("VerifyPathInput").Text=truncatedFile;
    Invoke("VerifyBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<TextBox>("DataOutput").Text!.Contains("exit code 1"),"truncated verify did not report the exit code");
    Check(Control<TextBox>("DataOutput").Text!.Contains("verify failed"),"truncated verify did not show the raw stderr");
    Check(Control<TextBlock>("DataStatus2").Text!.Contains("校验失败"),"truncated verify failure not reported");
    Control<TextBox>("BackupPathInput").Text=System.IO.Path.Combine(dataTemp,"missing-dir","a.gmbackup");
    Invoke("ExportBackupClick",null,null);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(window)!);
    Check(Control<TextBox>("DataOutput").Text!.Contains("目标目录不存在"),"missing target directory was not refused before calling the CLI");
    Check(!System.IO.Directory.Exists(System.IO.Path.Combine(dataTemp,"missing-dir")),"refused export created a directory");
    var stillThere=Post("api/v1/search",new{scope="all",include_retracted=true})["total"]!.GetValue<int>();
    Check(stillThere==beforeExport,"data panel operations changed the workspace record count");

    // ===== P3-A 草稿持久化与崩溃恢复 =====
    // 草稿根目录固定成 GM_DRAFT_DIR：子进程探针继承同一环境变量，才能验证“跨进程读回”。
    var draftRoot=System.IO.Path.Combine(System.IO.Path.GetTempPath(),"gm-drafts-"+Guid.NewGuid().ToString("N"));
    System.IO.Directory.CreateDirectory(draftRoot);
    Environment.SetEnvironmentVariable("GM_DRAFT_DIR",draftRoot);
    var ownExe=System.Reflection.Assembly.GetEntryAssembly()!.Location;
    var draftHostDirectory=System.IO.Path.Combine(System.IO.Path.GetTempPath(),"gm-draft-host-"+Guid.NewGuid().ToString("N"));
    var draftHost=new ServiceHost();
    MainWindow? draftWindow=null;
    var draftTrace=System.IO.Path.Combine(System.IO.Path.GetTempPath(),"gm-p3-trace-"+Guid.NewGuid().ToString("N")+".log");
    void Trace(string stage){try{System.IO.File.AppendAllText(draftTrace,DateTime.UtcNow.ToString("HH:mm:ss.fff")+" "+stage+"\n");}catch{}}
    Trace("start");
    // 探针子进程写完后会停住等父进程 Kill(整棵树) 来模拟崩溃，整段必须有硬超时，否则失败会留下孤儿进程。
    var draftDeadline=DateTime.UtcNow.AddMinutes(4);
    var draftWatchdog=new Thread(()=>{while(DateTime.UtcNow<draftDeadline)Thread.Sleep(500);Trace("watchdog timeout");Environment.Exit(4);}){IsBackground=true};
    draftWatchdog.Start();
    try{
        Trace("host service");
        // EnsureAsync 内部有 await：在 headless UI 线程上同步等待会因为没有 Dispatcher 泵而自锁，必须放到线程池上跑。
        var draftUrl=Task.Run(()=>draftHost.EnsureAsync(["--data-dir",draftHostDirectory,"--service-exe",binary])).GetAwaiter().GetResult();
        Trace("host up "+draftUrl);
        using var draftClient=new ServiceClient(draftUrl);
        var otherDirectory=System.IO.Path.Combine(System.IO.Path.GetTempPath(),"gm-draft-other-"+Guid.NewGuid().ToString("N"));
        var otherHost=new ServiceHost();
        Trace("other service");
        var otherUrl=Task.Run(()=>otherHost.EnsureAsync(["--data-dir",otherDirectory,"--service-exe",binary])).GetAwaiter().GetResult();
        Trace("other up "+otherUrl);
        try{
            var hostDataDir=draftClient.GetAsync("api/service").GetAwaiter().GetResult()["data_dir"]!.GetValue<string>();
            // 让主窗口走真实的打开路径（OnOpened -> Connect），data_dir 必须由 Connect 自己从服务取回。
            typeof(MainWindow).Assembly.GetType("GraphMemory.Desktop.Program")!.GetField("Arguments",BindingFlags.Static|BindingFlags.Public)!.SetValue(null,new[]{"--service-url",draftUrl});
            draftWindow=new MainWindow();
            typeof(MainWindow).GetField("client",flags)!.SetValue(draftWindow,new ServiceClient(draftUrl));
            typeof(MainWindow).GetField("dataDir",flags)!.SetValue(draftWindow,hostDataDir);
            typeof(MainWindow).GetField("draftsLoaded",flags)!.SetValue(draftWindow,true);
            Trace("show window");
            draftWindow.Show();Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(draftWindow)!);
            Trace("window ready");
            TextBox DraftBox(string name)=>draftWindow.FindControl<TextBox>(name)!;
            TextBlock DraftLabel(string name)=>draftWindow.FindControl<TextBlock>(name)!;
            void ClickDraft(string method){typeof(MainWindow).GetMethod(method,flags)!.Invoke(draftWindow,[null,null]);Pump(()=>!(bool)typeof(MainWindow).GetField("busy",flags)!.GetValue(draftWindow)!);}
            JsonNode LocalPost(string path,object body){using var c=new ServiceClient(draftUrl);return c.PostAsync(path,body).GetAwaiter().GetResult();}
            string DraftFileHere()=>DraftStore.FileFor(hostDataDir);

            // 面板显示的是规范化后的路径：带 \\?\ 前缀不应出现在屏幕上（与 ServiceHost 比较时同一归一做法）。
            object? NormalizeForDisplay(string path)=>typeof(MainWindow).GetMethod("NormalizePathForDisplay",flags|BindingFlags.Static)!.Invoke(null,[path]);
            var prefixed=@"\\?\C:\Users\someone\GraphMemory\workspace-v1";
            Check((string)NormalizeForDisplay(prefixed)!=System.IO.Path.GetFullPath(@"C:\definitely\not\this\path"),"display normalisation did not return the plain absolute path");
            Check((string)NormalizeForDisplay(prefixed)==System.IO.Path.GetFullPath(@"C:\Users\someone\GraphMemory\workspace-v1"),"\\?\\ prefix was not normalised for display: "+NormalizeForDisplay(prefixed));
            Check(!((string)NormalizeForDisplay(prefixed)!).Contains(@"\\?\"),"normalised display path still shows \\?\\");
            Check((string)NormalizeForDisplay(@"C:\temp\..\workspace")==System.IO.Path.GetFullPath(@"C:\workspace"),"plain path was broken by display normalisation: "+NormalizeForDisplay(@"C:\temp\..\workspace"));
            Check(NormalizeForDisplay("") as string=="","empty path was not preserved by display normalisation");
            var keepDataDir=typeof(MainWindow).GetField("dataDir",flags)!.GetValue(draftWindow);
            typeof(MainWindow).GetField("dataDir",flags)!.SetValue(draftWindow,prefixed);
            typeof(MainWindow).GetMethod("RefreshDataPanel",flags)!.Invoke(draftWindow,null);
            Check(!DraftLabel("DataDirText").Text!.Contains(@"\\?\"),"the data panel still shows a \\?\\ path: "+DraftLabel("DataDirText").Text);
            Check(DraftLabel("DataDirText").Text!.Contains(System.IO.Path.GetFullPath(@"C:\Users\someone\GraphMemory\workspace-v1")),"the data panel did not show the normalised path: "+DraftLabel("DataDirText").Text);
            typeof(MainWindow).GetField("dataDir",flags)!.SetValue(draftWindow,keepDataDir);
            typeof(MainWindow).GetMethod("RefreshDataPanel",flags)!.Invoke(draftWindow,null);

            DraftBox("TitleInput").Text="崩溃草稿标题";
            DraftBox("ContentInput").Text="崩溃前写下的正文";
            DraftBox("SourceInput").Text="来源：会话";
            DraftBox("ConditionsInput").Text="条件：Windows";
            typeof(MainWindow).GetMethod("SetKind",flags)!.Invoke(draftWindow,["decision"]);
            typeof(MainWindow).GetMethod("SetEvidence",flags)!.Invoke(draftWindow,["user_confirmed"]);
            Trace("persist");
            var savedNote=typeof(MainWindow).GetMethod("PersistDrafts",flags)!.Invoke(draftWindow,null) as string;
            Check(savedNote==null,"persisting a draft failed: "+savedNote);
            Check(System.IO.File.Exists(DraftFileHere()),"draft file was not created under GM_DRAFT_DIR");
            Check(DraftFileHere().StartsWith(draftRoot,StringComparison.OrdinalIgnoreCase),"draft file was written outside GM_DRAFT_DIR");
            Check(System.IO.Directory.GetFileSystemEntries(hostDataDir).All(x=>!System.IO.Path.GetFileName(x).Contains("draft")),"a draft file leaked into --data-dir");
            Check(System.IO.Directory.GetFiles(hostDataDir,"*",System.IO.SearchOption.AllDirectories).All(x=>!x.EndsWith(".tmp",StringComparison.OrdinalIgnoreCase)),"a draft temp file leaked into --data-dir");
            Check(System.IO.Directory.GetFiles(draftRoot,"*.tmp").Length==0,"atomic write left a temporary file behind");
            var loaded=DraftStore.Load(hostDataDir);
            Check(loaded.Note==null&&loaded.Drafts.Count==1,"a single-scope draft bundle did not reload exactly once");
            var record=loaded.Drafts.Values.Single();
            Check(record.Title=="崩溃草稿标题"&&record.Content=="崩溃前写下的正文"&&record.Source=="来源：会话"&&record.Conditions=="条件：Windows"&&record.Kind=="decision"&&record.Evidence=="user_confirmed","persisted draft fields did not round-trip");
            Check(record.IsNew&&record.MemoryId==null,"a brand new record draft must not carry a memory id");
            Check(record.Version==0&&record.Scope=="personal","new draft baseline/scope wrong");
            Console.WriteLine("OK persisted draft round-trips file -> DraftStore with all fields");

            // 真实双进程：子进程 A 写草稿后被 Kill(整棵树)（不给它任何收尾机会），父进程再读回。
            Trace("spawn writer parentBucket="+DraftStore.BucketName(hostDataDir)+" parentDir="+DraftStore.Normalize(hostDataDir)+" argDir="+DraftStore.Normalize(draftHostDirectory));
            var writer=System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("dotnet"){
                UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true,
                ArgumentList={ownExe,"--draft-probe","write",draftHostDirectory,draftRoot,"子进程草稿","kill 前写入的正文","来源：子进程","条件：kill","decision","user_confirmed"},
                WorkingDirectory=System.IO.Path.GetFullPath("."),
            })!;
            var written=false;var writtenDeadline=DateTime.UtcNow.AddSeconds(60);
            while(!written&&DateTime.UtcNow<writtenDeadline){
                var line=writer.StandardOutput.ReadLine();
                if(line==null)break;
                if(line.Contains("DRAFT_PROBE_WRITTEN"))written=true;
                else if(line.Contains("DRAFT_PROBE_FAILED"))throw new Exception("draft probe child failed: "+line);
                else if(line.Contains("DRAFT_PROBE_FILE"))Trace("child "+line);
            }
            Trace("kill writer written="+written);
            writer.Kill(true);writer.WaitForExit(15000);
            Check(written,"child process did not confirm writing the draft: "+writer.StandardError.ReadToEnd());
            // 只证明“文件在”不够：必须能从磁盘解析出被杀进程写下的字段（JSON 里中文会被转义，所以走解析而不是字符串匹配）。
            var fromChild=DraftStore.Load(hostDataDir);
            Check(fromChild.Drafts.Count==1&&fromChild.Drafts["personal"].Title=="子进程草稿"&&fromChild.Drafts["personal"].Content=="kill 前写入的正文","killed child's draft was not readable from disk");
            Check(System.IO.File.ReadAllText(DraftFileHere(),System.Text.Encoding.UTF8).Contains("\"title\""),"the draft file on disk has no title field");
            // 反向证据：什么都不做的子进程被杀后，磁盘上不会凭空出现草稿文件。
            var emptyBucket=DraftStore.FileFor(System.IO.Path.Combine(draftHostDirectory,"never-created"));
            System.IO.File.Delete(emptyBucket);
            var hang=System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("dotnet"){
                UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true,
                ArgumentList={ownExe,"--draft-probe","hang",draftHostDirectory,draftRoot},
                WorkingDirectory=System.IO.Path.GetFullPath("."),
            })!;
            var hanging=false;var hangDeadline=DateTime.UtcNow.AddSeconds(60);
            while(!hanging&&DateTime.UtcNow<hangDeadline){var line=hang.StandardOutput.ReadLine();if(line==null)break;if(line.Contains("DRAFT_PROBE_HANGING"))hanging=true;}
            hang.Kill(true);hang.WaitForExit(15000);
            Check(hanging,"hang probe did not report readiness");
            Check(!System.IO.File.Exists(emptyBucket),"a killed process with no draft wrote a draft file anyway");
            Trace("reopen");
            var reopened=new MainWindow();
            typeof(MainWindow).GetField("client",flags)!.SetValue(reopened,new ServiceClient(draftUrl));
            // 真实重启路径：只给服务 URL，data_dir 与草稿都必须由 Connect 自己取回。
            Complete((Task)typeof(MainWindow).GetMethod("Connect",flags)!.Invoke(reopened,null)!);
            Check((bool)typeof(MainWindow).GetMethod("ApplyDraftToEditor",flags)!.Invoke(reopened,null)!,"new process did not restore the child's draft");
            Check(reopened.FindControl<TextBox>("TitleInput")!.Text=="子进程草稿","new process restored the wrong title");
            Check(reopened.FindControl<TextBox>("ContentInput")!.Text=="kill 前写入的正文","new process restored the wrong content");
            Check(reopened.FindControl<TextBox>("SourceInput")!.Text=="来源：子进程"&&reopened.FindControl<TextBox>("ConditionsInput")!.Text=="条件：kill","new process restored the wrong source/conditions");
            Check(reopened.FindControl<ComboBox>("Kind")!.SelectedIndex==2,"new process restored the wrong kind");
            Check(reopened.FindControl<ComboBox>("EvidenceInput")!.SelectedIndex==1,"new process restored the wrong evidence");
            Check(reopened.FindControl<TextBlock>("DraftNotice")!.IsVisible&&reopened.FindControl<TextBlock>("DraftNotice")!.Text!.Contains("上次未保存"),"recovered draft was not labelled as unsaved");
            reopened.Close();
            Console.WriteLine("OK crash recovery #1: child wrote with a real data_dir, was killed, and a new window read every field back");
            // 关掉这一扇窗的草稿自动落盘：它的 500ms 防抖若在后面 Pump 时才触发，会把旧内容盖回磁盘，掩盖子进程写的草稿。
            ((DispatcherTimer)typeof(MainWindow).GetField("draftTimer",flags)!.GetValue(draftWindow)!).Stop();
            typeof(MainWindow).GetField("draftsLoaded",flags)!.SetValue(draftWindow,false);

            // 真实双进程 #2：子进程预先不知道 data_dir，也从不打开数据面板，只靠 --service-url 连接后输入并落盘。
            // 这是“草稿恢复不能依赖用户打开过数据面板”的直接回归点，且本循环不预置任何草稿文件。
            var hostRealDir=DraftStore.Normalize(hostDataDir);
            var thirdTitle="面板未开草稿标题";
            var thirdContent="面板未开草稿正文";
            DraftStore.DeleteBucket(hostRealDir);
            DraftStore.DeleteBucket("");
            Check(!System.IO.File.Exists(DraftStore.FileFor(hostRealDir)),"cycle 2 did not start from an empty workspace bucket");
            Trace("cycle2 spawn connect-writer");
            var third=System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("dotnet"){
                UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true,
                ArgumentList={ownExe,"--draft-probe","connect-write",draftUrl,draftRoot,thirdTitle,thirdContent},
                WorkingDirectory=System.IO.Path.GetFullPath("."),
            })!;
            var thirdWritten=false;var thirdDeadline=DateTime.UtcNow.AddSeconds(90);
            while(!thirdWritten&&DateTime.UtcNow<thirdDeadline){
                var line=third.StandardOutput.ReadLine();
                if(line==null)break;
                if(line.Contains("DRAFT_PROBE_WRITTEN"))thirdWritten=true;
                else if(line.Contains("DRAFT_PROBE_FAILED"))throw new Exception("connect-write child failed: "+line);
            }
            Trace("cycle2 kill written="+thirdWritten);
            third.Kill(true);third.WaitForExit(15000);
            Check(thirdWritten,"connect-write child did not confirm writing the draft: "+third.StandardError.ReadToEnd());
            Check(System.IO.File.Exists(DraftStore.FileFor(hostRealDir)),"the draft did not land in the real workspace bucket while the data panel stayed closed");
            Check(DraftStore.Load(hostRealDir).Drafts.Count==1,"the real workspace bucket does not hold exactly one draft");
            Check(!System.IO.File.Exists(DraftStore.FileFor("")),"a draft was written into the shared gm-draft-no-workspace bucket");
            Trace("cycle2 reopen");
            var thirdReopened=new MainWindow();
            typeof(MainWindow).GetField("client",flags)!.SetValue(thirdReopened,new ServiceClient(draftUrl));
            Complete((Task)typeof(MainWindow).GetMethod("Connect",flags)!.Invoke(thirdReopened,null)!);
            var connectedDir=typeof(MainWindow).GetField("dataDir",flags)!.GetValue(thirdReopened) as string;
            Check(connectedDir==hostDataDir,"Connect did not fetch the real data_dir from the service: "+connectedDir);
            Check((bool)typeof(MainWindow).GetMethod("ApplyDraftToEditor",flags)!.Invoke(thirdReopened,null)!,"a new window did not restore a draft written without opening the data panel");
            Check(thirdReopened.FindControl<TextBox>("TitleInput")!.Text==thirdTitle,"recovered title is wrong: "+thirdReopened.FindControl<TextBox>("TitleInput")!.Text);
            Check(thirdReopened.FindControl<TextBox>("ContentInput")!.Text==thirdContent,"recovered content is wrong: "+thirdReopened.FindControl<TextBox>("ContentInput")!.Text);
            Check(thirdReopened.FindControl<TextBlock>("DraftNotice")!.IsVisible&&thirdReopened.FindControl<TextBlock>("DraftNotice")!.Text!.Contains("上次未保存"),"recovered draft was not labelled as unsaved");
            Check(!System.IO.File.Exists(DraftStore.FileFor("")),"connecting and restoring created a gm-draft-no-workspace bucket");
            thirdReopened.Close();
            Console.WriteLine("OK crash recovery #2: a child that never opened the data panel persisted into the real workspace bucket, and Connect restored it");

            // 真实双进程 #3：进程被杀后重启，用户不做任何额外操作就直接继续编辑，草稿仍在同一工作库分片里。
            Trace("cycle3 spawn connect-continue");
            var fourth=System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("dotnet"){
                UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true,
                ArgumentList={ownExe,"--draft-probe","connect-continue",draftUrl,draftRoot,"被续写的草稿","续写后的正文"},
                WorkingDirectory=System.IO.Path.GetFullPath("."),
            })!;
            var continued=false;var fourthDeadline=DateTime.UtcNow.AddSeconds(90);
            while(!continued&&DateTime.UtcNow<fourthDeadline){
                var line=fourth.StandardOutput.ReadLine();
                if(line==null)break;
                if(line.Contains("DRAFT_PROBE_WRITTEN"))continued=true;
                else if(line.Contains("DRAFT_PROBE_FAILED"))throw new Exception("connect-continue child failed: "+line);
            }
            Trace("cycle3 kill continued="+continued);
            fourth.Kill(true);fourth.WaitForExit(15000);
            Check(continued,"connect-continue child did not confirm writing the draft: "+fourth.StandardError.ReadToEnd());
            var continuedLoad=DraftStore.Load(hostRealDir);
            Check(continuedLoad.Drafts.Count==1,"a plain restart + edit left more than one draft in the workspace bucket");
            Check(continuedLoad.Drafts["personal"].Title=="被续写的草稿"&&continuedLoad.Drafts["personal"].Content=="续写后的正文","a plain restart + edit did not keep the draft in the real workspace bucket");
            Check(!System.IO.File.Exists(DraftStore.FileFor("")),"cycle 3 wrote into the shared gm-draft-no-workspace bucket");
            Console.WriteLine("OK crash recovery #3: after a kill, connecting alone is enough to keep editing in the same workspace bucket");

            // 不同 data_dir 隔离：分片哈希稳定，互不可见。
            Trace("isolation");
            var otherDataDir=new ServiceClient(otherUrl).GetAsync("api/service").GetAwaiter().GetResult()["data_dir"]!.GetValue<string>();
            Check(otherDataDir!=hostDataDir,"the two test workspaces share a data_dir");
            Check(DraftStore.FileFor(hostDataDir)!=DraftStore.FileFor(otherDataDir),"two workspaces share one draft bucket");
            // 两个工作库各写一份草稿：必须互不可见（此前测的是“另一个工作库没有草稿文件”）。
            Check(DraftStore.Save(otherDataDir,[new DraftRecord("personal",null,0,null,"note","user_or_agent_statement","另一个工作库草稿","只属于另一个工作库","","",true,"other-event","")])==null,"saving a draft for the second workspace failed");
            Check(System.IO.File.Exists(DraftStore.FileFor(otherDataDir)),"the second workspace has no draft file after saving one");
            Check(DraftStore.Load(otherDataDir).Drafts.Count==1&&DraftStore.Load(otherDataDir).Drafts["personal"].Title=="另一个工作库草稿","the second workspace cannot read back its own draft");
            Check(DraftStore.Load(hostDataDir).Drafts.Values.All(x=>x.Title!="另一个工作库草稿"),"the second workspace's draft is visible from the first workspace");
            Check(DraftStore.Load(hostDataDir).Drafts["personal"].Title=="被续写的草稿","the first workspace lost its own draft while testing isolation");
            Check(DraftStore.BucketName(hostDataDir)==DraftStore.BucketName(hostDataDir.ToUpperInvariant().Replace('/','\\')),"draft bucket hash is not stable across path spellings");
            Check(DraftStore.BucketName(hostDataDir)!=DraftStore.BucketName(otherDataDir),"different data_dir values got the same bucket name");
            Check(DraftStore.BucketName(hostDataDir)==DraftStore.BucketName(@"\\?\"+DraftStore.Normalize(hostDataDir)),"\\?\\ prefix produced a second bucket for the same workspace");
            Console.WriteLine("OK workspace isolation: different data_dir -> different bucket, one workspace -> one bucket");

            // 损坏文件：半截 JSON / 非 JSON / 空文件 / 字段类型不对 → 空集 + 可读说明，不崩溃、不抛异常。
            Trace("corrupt");
            var goodText=System.IO.File.ReadAllText(DraftStore.FileFor(hostDataDir),System.Text.Encoding.UTF8);
            System.IO.File.WriteAllText(DraftStore.FileFor(hostDataDir),goodText[..(goodText.Length/2)],System.Text.Encoding.UTF8);
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            Check((typeof(MainWindow).GetField("drafts",flags)!.GetValue(draftWindow) as System.Collections.IDictionary)!.Count==0,"half JSON did not degrade to an empty draft set");
            Check(DraftLabel("DraftNotice").IsVisible&&DraftLabel("DraftNotice").Text!.Contains("损坏"),"half JSON did not explain the degradation");
            Check(DraftStore.Load(hostDataDir).Drafts.Count==0,"half JSON produced a draft");
            System.IO.File.WriteAllText(DraftStore.FileFor(hostDataDir),"这不是 JSON {{{",System.Text.Encoding.UTF8);
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            Check(DraftLabel("DraftNotice").Text!.Contains("损坏"),"non-JSON draft file did not explain the degradation");
            Check(DraftStore.Load(hostDataDir).Note!=null,"non-JSON load reported no note");
            System.IO.File.WriteAllText(DraftStore.FileFor(hostDataDir),"",System.Text.Encoding.UTF8);
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            Check((typeof(MainWindow).GetField("drafts",flags)!.GetValue(draftWindow) as System.Collections.IDictionary)!.Count==0,"empty draft file did not degrade to an empty set");
            Check(DraftLabel("DraftNotice").IsVisible,"empty draft file produced no explanation");
            System.IO.File.WriteAllText(DraftStore.FileFor(hostDataDir),$"{{\"format\":1,\"data_dir\":{System.Text.Json.JsonSerializer.Serialize(DraftStore.Normalize(hostDataDir))},\"drafts\":{{\"personal\":{{\"scope\":\"personal\",\"is_new\":true,\"version\":\"not-a-number\",\"title\":\"x\",\"content\":\"y\"}}}}}}",System.Text.Encoding.UTF8);
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            Check(DraftLabel("DraftNotice").Text!.Contains("损坏"),"wrong field types did not degrade to an empty draft set");
            Check(DraftStore.Load(hostDataDir).Drafts.Count==0,"wrong field types produced a draft instead of an empty set");
            // 损坏文件的原文必须保留（用户可自查），只是被按“没有草稿”处理。
            Check(System.IO.File.Exists(DraftStore.FileFor(hostDataDir)),"corrupt draft file was deleted instead of kept for inspection");
            System.IO.File.WriteAllText(DraftStore.FileFor(hostDataDir),goodText,System.Text.Encoding.UTF8);
            Console.WriteLine("OK corrupted draft files (truncated / non-JSON / empty / wrong types) degrade to empty with a readable note");

            // 放弃草稿：条目消失、文件删除、给出确认。
            Trace("discard");
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            Check(DraftStore.Load(hostDataDir).Drafts.Count==1,"restoring a good draft file failed");
            // 空 data_dir 必须被当作“工作库未知”，绝不落进共享的无工作库分片。
            var emptySaveNote=DraftStore.Save("",[new DraftRecord("personal",null,0,null,"note","user_or_agent_statement","不该落盘的草稿","无工作库","","",true,"e","")]);
            Check(emptySaveNote!=null,"saving a draft with an unknown workspace directory was accepted");
            Check(emptySaveNote!.Contains("工作库目录未知"),"unknown-workspace refusal is not explained: "+emptySaveNote);
            Check(!System.IO.File.Exists(DraftStore.FileFor("")),"an unknown data_dir still produced a draft file");
            typeof(MainWindow).GetField("dataDir",flags)!.SetValue(draftWindow,"");
            var unknownNote=typeof(MainWindow).GetMethod("PersistDrafts",flags)!.Invoke(draftWindow,null) as string;
            Check(unknownNote!=null&&DraftLabel("DraftNotice").Text!.Contains("工作库目录未知"),"saving while the workspace directory is unknown gave no visible warning");
            typeof(MainWindow).GetField("dataDir",flags)!.SetValue(draftWindow,hostDataDir);
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            typeof(MainWindow).GetMethod("DiscardDraftClick",flags)!.Invoke(draftWindow,[null,null]);
            Check(DraftStore.Load(hostDataDir).Drafts.Count==0,"discarded draft is still persisted");
            Check(!System.IO.File.Exists(DraftStore.FileFor(hostDataDir)),"discard left the draft file behind");
            Check(DraftLabel("DraftNotice").Text!.Contains("已放弃"),"discard gave no user-visible confirmation");
            Check(!(bool)typeof(MainWindow).GetMethod("HasDraft",flags)!.Invoke(draftWindow,null)!,"discarded draft is still treated as a draft");
            Console.WriteLine("OK discarding a draft removes the persisted entry, the file, and the editor state");

            // 版本过期提示 + 409 保留草稿。
            Trace("stale");
            var beforeStale=LocalPost("api/v1/search",new{scope="all",include_retracted=true})["memories"]!.AsArray().Select(x=>x!["id"]!.GetValue<string>()).ToHashSet();
            var staleMemory=LocalPost("api/v1/memories",new{memory_scope="personal",event_id=Guid.NewGuid().ToString(),title="过期基线",content="基线正文"})["memory"]!;
            var staleId=staleMemory["id"]!.GetValue<string>();
            Check(!beforeStale.Contains(staleId),"stale baseline id was not fresh");
            LocalPost("api/v1/memories/update",new{id=staleId,expected_version=1,content="他人的中间更新"});   // 基线草稿仍是 v1，服务端已是 v2
            Check(LocalPost("api/v1/search",new{scope="all",include_retracted=true})["memories"]!.AsArray().Single(x=>x!["id"]!.GetValue<string>()==staleId)!["version"]!.GetValue<int>()==2,"stale setup is wrong");
            var staleDraft=new DraftRecord("personal",staleId,1,null,"note","user_or_agent_statement","过期基线","草稿里的正文","来源：会话","",false,"draft-event","");
            Check(DraftStore.Save(hostDataDir,[staleDraft])==null,"saving the baseline draft failed");
            typeof(MainWindow).GetMethod("LoadPersistedDrafts",flags)!.Invoke(draftWindow,null);
            Check((bool)typeof(MainWindow).GetMethod("ApplyDraftToEditor",flags)!.Invoke(draftWindow,null)!,"baseline draft was not restored into the editor");
            Check(draftWindow.FindControl<Button>("DiscardDraftButton")!.IsVisible,"restored draft does not offer a discard action");
            DraftBox("ContentInput").Text="我改过的正文";   // 用户在恢复出来的草稿上继续编辑（这样才与 v1 基线不同）
            Check((bool)typeof(MainWindow).GetMethod("HasDraft",flags)!.Invoke(draftWindow,null)!,"an edited restored draft is not treated as a draft");
            Complete((Task)typeof(MainWindow).GetMethod("CheckDraftBaselineAsync",flags)!.Invoke(draftWindow,null)!);
            Check(DraftLabel("DraftNotice").Text!.Contains("已被其他会话更新"),"stale baseline did not warn: "+DraftLabel("DraftNotice").Text);
            Check(DraftLabel("DraftNotice").Text!.Contains("重新核对"),"stale baseline warning is not actionable");
            var persistError=typeof(MainWindow).GetMethod("PersistDrafts",flags)!.Invoke(draftWindow,null) as string;
            Check(persistError==null,"resaving the stale draft failed: "+persistError);
            Trace("save click");
            ClickDraft("SaveClick");
            Trace("save done");
            Check(draftWindow.FindControl<Button>("ReloadButton")!.IsVisible,"saving a stale draft did not surface the 409 path: "+DraftLabel("Status").Text);
            Check(DraftBox("ContentInput").Text=="我改过的正文","409 conflict cleared the draft content");
            Check(DraftStore.Load(hostDataDir).Drafts.Count==1,"409 conflict deleted the persisted draft entry");
            Trace("reload click");
            ClickDraft("ReloadClick");
            Trace("reload done");
            // 保存被 409 拒绝后，服务端必须仍停在别人写的 v2 上（草稿基线 v1 没有被强推成新版），
            // 而“放弃草稿并加载最新”必须显示真正的最新正文。
            var latestNow=LocalPost("api/v1/search",new{scope="all",include_retracted=true})["memories"]!.AsArray().Single(x=>x!["id"]!.GetValue<string>()==staleId)!;
            Check(latestNow["version"]!.GetValue<int>()==2,"a rejected save still overwrote someone else's update: version is now "+latestNow["version"]!.GetValue<int>());
            Check(DraftBox("ContentInput").Text==latestNow["content"]!.GetValue<string>(),"reload did not load the newest version; got ["+DraftBox("ContentInput").Text+"] expected ["+latestNow["content"]!.GetValue<string>()+"]");
            Check(DraftStore.Load(hostDataDir).Drafts.Count==0,"reload did not clear the abandoned persisted draft");
            Console.WriteLine("OK stale baseline warns, 409 keeps the persisted draft, reload clears it");
        } finally { otherHost.StopOwned(); }
    } finally {
        draftDeadline=DateTime.UtcNow;
        draftWindow?.Close();
        draftHost.StopOwned();
        try{System.IO.Directory.Delete(draftRoot,true);}catch{}
        try{System.IO.Directory.Delete(draftHostDirectory,true);}catch{}
        Trace("done");
    }
    Console.WriteLine($"PASS {checks} editor checks using real XAML controls + isolated HTTP service");
} finally {
    window?.Close();host.StopOwned();
}

// P3-A 双进程探针：`--draft-probe write|connect-write|connect-continue|read|hang|host …`。
// write 落盘后打印 DRAFT_PROBE_WRITTEN 并原地停住（不占 CPU），等父进程 Kill(整棵树) 模拟崩溃；
// connect-write / connect-continue 只拿 --service-url（从不打开数据面板、不知道 data_dir），
// 用真实 MainWindow 连接后输入并落盘，用来证明草稿恢复不依赖数据面板；
// hang 什么都不写就停住，用于证明“被杀进程不会凭空落盘”；read 读回并打印所有字段。
static class DraftProbe {
    static void Hold(){new ManualResetEventSlim(false).Wait(TimeSpan.FromMinutes(10));}
    public static void Run(string[] args) {
        if(args.Length<4){Console.WriteLine("DRAFT_PROBE_FAILED usage");return;}
        var mode=args[1];var dataDir=args[2];var draftRoot=args[3];
        Environment.SetEnvironmentVariable("GM_DRAFT_DIR",draftRoot);
        try {
            if(mode=="host"){   // 诊断用：单独验证 ServiceHost.EnsureAsync 的发现/启动路径
                var started=System.Diagnostics.Stopwatch.StartNew();
                try{var url=new ServiceHost().EnsureAsync(["--data-dir",dataDir,"--service-exe",draftRoot]).GetAwaiter().GetResult();
                    Console.WriteLine("DRAFT_PROBE_HOST "+url+" in "+started.ElapsedMilliseconds+"ms");}
                catch(Exception e){Console.WriteLine("DRAFT_PROBE_FAILED host "+e.GetType().Name+" "+e.Message+" after "+started.ElapsedMilliseconds+"ms");}
                return;
            }
            if(mode=="hang"){Console.WriteLine("DRAFT_PROBE_HANGING");Console.Out.Flush();Hold();return;}
            if(mode=="connect-write"||mode=="connect-continue"){
                // 只拿服务 URL，走真实 Connect 路径（data_dir 必须由 Connect 自己去取）；全程不打开数据面板。
                var url=dataDir;
                var title=args.Length>4?args[4]:"面板未开草稿标题";
                var content=args.Length>5?args[5]:"面板未开草稿正文";
                var flags=BindingFlags.Instance|BindingFlags.NonPublic;
                typeof(MainWindow).Assembly.GetType("GraphMemory.Desktop.Program")!.GetField("Arguments",BindingFlags.Static|BindingFlags.Public)!.SetValue(null,new[]{"--service-url",url});
                AppBuilder.Configure<App>().UseHeadless(new AvaloniaHeadlessPlatformOptions()).SetupWithoutStarting();
                var window=new MainWindow();
                typeof(MainWindow).GetField("client",flags)!.SetValue(window,new ServiceClient(url));
                var connect=(Task)typeof(MainWindow).GetMethod("Connect",flags)!.Invoke(window,null)!;
                var deadline=DateTime.UtcNow.AddSeconds(60);
                while(!connect.IsCompleted&&DateTime.UtcNow<deadline){Dispatcher.UIThread.RunJobs();Thread.Sleep(10);}
                if(!connect.IsCompleted)throw new Exception("probe Connect timed out");
                connect.GetAwaiter().GetResult();
                var resolved=typeof(MainWindow).GetField("dataDir",flags)!.GetValue(window) as string;
                if(string.IsNullOrWhiteSpace(resolved))throw new Exception("Connect left data_dir empty");
                if(mode=="connect-write"){
                    if((bool)typeof(MainWindow).GetMethod("ApplyDraftToEditor",flags)!.Invoke(window,null)!)
                        Console.WriteLine("DRAFT_PROBE_NOTE an existing draft was restored before writing");
                }
                window.FindControl<TextBox>("TitleInput")!.Text=title;
                window.FindControl<TextBox>("ContentInput")!.Text=content;
                var note=typeof(MainWindow).GetMethod("PersistDrafts",flags)!.Invoke(window,null) as string;   // 直接落盘，不等 500ms 防抖
                if(note!=null){Console.WriteLine("DRAFT_PROBE_FAILED persist "+note);return;}
                if(!System.IO.File.Exists(DraftStore.FileFor(resolved))){Console.WriteLine("DRAFT_PROBE_FAILED no file for "+resolved);return;}
                Console.WriteLine("DRAFT_PROBE_WRITTEN");Console.Out.Flush();
                Hold();
                return;
            }
            if(mode=="write") {
                string Arg(int i,string fallback)=>args.Length>i?args[i]:fallback;
                var record=new DraftRecord("personal",null,0,null,Arg(8,"decision"),Arg(9,"user_confirmed"),Arg(4,"子进程草稿"),Arg(5,"kill 前写入的正文"),Arg(6,"来源：子进程"),Arg(7,"条件：kill"),true,"child-event","");
                var error=DraftStore.Save(dataDir,[record]);
                if(error!=null){Console.WriteLine("DRAFT_PROBE_FAILED "+error);return;}
                if(!System.IO.File.Exists(DraftStore.FileFor(dataDir))){Console.WriteLine("DRAFT_PROBE_FAILED no file");return;}
                Console.WriteLine("DRAFT_PROBE_FILE "+DraftStore.BucketName(dataDir)+" <- "+DraftStore.Normalize(dataDir));
                Console.WriteLine("DRAFT_PROBE_WRITTEN");Console.Out.Flush();
                Hold();
                return;
            }
            var loaded=DraftStore.Load(dataDir);
            var value=loaded.Drafts.TryGetValue("personal",out var found)?found:null;
            Console.WriteLine(new JsonObject{
                ["note"]=loaded.Note,["count"]=loaded.Drafts.Count,
                ["title"]=value?.Title,["content"]=value?.Content,["source"]=value?.Source,
                ["conditions"]=value?.Conditions,["kind"]=value?.Kind,["evidence"]=value?.Evidence,["is_new"]=value?.IsNew,
            }.ToJsonString());
        } catch(Exception e) { Console.WriteLine("DRAFT_PROBE_FAILED "+e.Message); }
    }
}
