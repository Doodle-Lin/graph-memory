using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Threading;

namespace GraphMemory.Desktop;
// P3-A 草稿持久化与崩溃恢复：草稿落到工作库之外的 DraftStore，进程被杀也能读回。
// 草稿不进入记忆库：全程只走本地文件，不调用任何 HTTP/MCP 接口。
public partial class MainWindow {
    readonly DispatcherTimer draftTimer=new(){Interval=TimeSpan.FromMilliseconds(500)};
    bool draftsLoaded;
    string draftNotice="";
    // 当前 scope 的键：项目 id 或 personal（与既有 drafts 表一致）。
    string DraftScopeKey()=>project?.Id??"personal";
    string DraftFile()=>DraftStore.FileFor(dataDir);
    // 保存失败/损坏说明都会写进编辑器顶部的说明条，绝不静默。
    void SetDraftNotice(string text){
        draftNotice=text;
        var label=Find<TextBlock>("DraftNotice");if(label==null)return;
        label.IsVisible=text!="";label.Text=text;
    }
    void ScheduleDraftSave(){if(!draftsLoaded)return;draftTimer.Stop();draftTimer.Start();}
    // 初始化：输入变化后 500ms 落盘一次，所以“打字后进程被杀”也能恢复；不从关窗动作依赖。
    void InitializeDraftPersistence(){
        foreach(var name in new[]{"TitleInput","ContentInput","SourceInput","ConditionsInput"}){
            var box=Find<TextBox>(name);
            if(box!=null)box.TextChanged+=(_,_)=>ScheduleDraftSave();
        }
        var kind=Find<ComboBox>("Kind");if(kind!=null)kind.SelectionChanged+=(_,_)=>ScheduleDraftSave();
        var evidence=Find<ComboBox>("EvidenceInput");if(evidence!=null)evidence.SelectionChanged+=(_,_)=>ScheduleDraftSave();
        draftTimer.Tick+=(_,_)=>{draftTimer.Stop();PersistDrafts();};
    }
    // 把当前编辑器内容并入 drafts 表（与既有 SnapshotCurrentDraft 语义一致），再整体落盘。
    string? PersistDrafts(){
        if(!draftsLoaded)return null;
        var key=DraftScopeKey();
        if(HasDraft())drafts[key]=(Text("TitleInput"),Text("ContentInput"),Text("SourceInput"),Text("ConditionsInput"),SelectedEvidence(),selected,origin,SelectedKind(),eventId,lastRequest);
        else drafts.Remove(key);
        if(drafts.Count==0){DraftStore.DeleteBucket(dataDir);return null;}
        var error=DraftStore.Save(dataDir,drafts.Select(pair=>ToDraftRecord(pair.Key,pair.Value)));
        if(error!=null)SetDraftNotice(error);   // 落盘失败必须让用户看见，不能静默丢草稿
        return error;
    }
    static DraftRecord ToDraftRecord(string scope,(string title,string content,string source,string conditions,string evidence,MemoryItem? memory,string? origin,string kind,string eventId,string request) draft)=>
        new(scope,draft.memory?.Id,draft.memory?.Version??0,draft.origin,draft.kind,draft.evidence,draft.title,draft.content,draft.source,draft.conditions,draft.memory==null,draft.eventId,draft.request);
    // 基线版本：草稿里的 memory_id 只带版本号，足够走既有的 expected_version 冲突流程。
    static MemoryItem BaselineMemory(DraftRecord record){
        var node=new System.Text.Json.Nodes.JsonObject{
            ["id"]=record.MemoryId,["memory_scope"]=record.Scope=="personal"?"personal":"project",
            ["project_id"]=record.Scope=="personal"?null:record.Scope,["version"]=record.Version,
            ["title"]=record.Title,["content"]=record.Content,["kind"]=record.Kind,
            ["source"]=record.Source,["conditions"]=record.Conditions,["evidence_type"]=record.Evidence,
        };
        return new MemoryItem(node);
    }
    // 启动/切 scope 时读取该工作库的草稿分片；损坏或缺席都只给一句说明，不阻塞启动。
    void LoadPersistedDrafts(){
        var result=DraftStore.Load(dataDir);
        drafts.Clear();
        foreach(var (scope,record) in result.Drafts)
            drafts[scope]=(record.Title,record.Content,record.Source,record.Conditions,record.Evidence,
                record.MemoryId==null?null:BaselineMemory(record),record.Origin,record.Kind,record.EventId,record.Request);
        draftsLoaded=true;
        var note=result.Note??"";
        if(note=="")note=drafts.ContainsKey(DraftScopeKey())?"这是上次未保存的草稿，未保存的编辑已经恢复。核对后再保存或放弃。":"";
        SetDraftNotice(note);
        RefreshDraftAffordances();
    }
    // 编辑器打开时把该 scope 的草稿内容填回控件（返回是否有草稿）。
    bool ApplyDraftToEditor(){
        if(!drafts.TryGetValue(DraftScopeKey(),out var draft))return false;
        Set("TitleInput",draft.title);Set("ContentInput",draft.content);Set("SourceInput",draft.source);Set("ConditionsInput",draft.conditions);
        selected=draft.memory;origin=draft.origin;eventId=draft.eventId;lastRequest=draft.request;
        SetKind(draft.kind);SetEvidence(draft.evidence);UpdateEvidenceHint();
        Find<Button>("UndoButton").IsVisible=selected?.Version>1;
        Find<Button>("PromoteButton").IsVisible=selected?.Scope=="project";
        Find<TextBlock>("EditorHeading").Text=selected==null?"继续草稿":"继续纠正（保存时校验版本）";
        RefreshDraftAffordances();
        return true;
    }
    void RefreshDraftAffordances(){
        var button=Find<Button>("DiscardDraftButton");
        if(button!=null)button.IsVisible=drafts.ContainsKey(DraftScopeKey());
        Find<Button>("NewMemoryButton").Content=HasDraft()||drafts.ContainsKey(DraftScopeKey())?"继续编辑":"记一条";
    }
    // 基线版本过期检查：当前版本 > 草稿基线时必须提示重读，且绝不静默覆盖他人更新。
    async Task CheckDraftBaselineAsync(){
        if(selected==null||client==null)return;
        try{
            var response=await Api().PostAsync("api/v1/search",new{project_id=selected.Scope=="personal"?null:selected.Text("project_id"),query="",include_retracted=true});
            var latest=response["memories"]!.AsArray().FirstOrDefault(x=>x!["id"]!.GetValue<string>()==selected.Id);
            if(latest==null)return;
            var version=latest["version"]!.GetValue<ulong>();
            if(version>selected.Version)SetDraftNotice($"记录已被其他会话更新（草稿基线 v{selected.Version}，当前 v{version}），草稿需重新核对；保存会走版本冲突流程，不会覆盖他人更新。可加载历史核对，或点“放弃这份草稿”。");
        }catch{ /* 基线检查失败不影响草稿恢复本身，保存时仍由 409 兜底 */ }
    }
    void DiscardDraftClick(object? s,RoutedEventArgs e){
        if(busy)return;
        var key=DraftScopeKey();
        var had=drafts.ContainsKey(key);
        drafts.Remove(key);
        var note=PersistDrafts();
        var removed=DraftStore.DeleteBucket(dataDir);
        ClearEditor();
        SetDraftNotice(had?"已放弃这份草稿，持久化条目已删除。":note??(removed?"已清理残留的草稿文件。":"当前没有草稿可放弃。"));
        RefreshDraftAffordances();
    }
    // 关闭窗口时“放弃并关闭”也要清掉持久化条目，否则下次启动又会恢复已放弃的草稿。
    void DiscardAllDrafts(){
        drafts.Clear();
        DraftStore.DeleteBucket(dataDir);
    }
}
