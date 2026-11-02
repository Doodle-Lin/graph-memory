using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Threading;

namespace GraphMemory.Desktop;
// L1-04 关闭保护：窗口关闭前拦截未保存草稿（当前 scope 与其他 scope 都算），
// 用窗口内模态层让用户选择“全部保存并关闭 / 放弃并关闭 / 取消”。
public partial class MainWindow {
    bool allowClose;
    string closeSaveNote="";
    void WindowClosing(object? sender,WindowClosingEventArgs e){
        if(allowClose)return;                       // 显式放弃或全部保存成功后才放行
        if(busy){e.Cancel=true;StatusMessage("当前操作尚未完成，请等待完成后再关闭窗口。");return;}
        if(PendingDraftCount()>0){e.Cancel=true;ShowClosePrompt();return;}
        // 没有未保存草稿：正常关闭，不停止后台服务。
    }
    void WindowKeyDown(object? sender,KeyEventArgs e){
        if(e.Key==Key.Escape&&Find<Border>("ClosePrompt").IsVisible){RequestCancelClose();e.Handled=true;}
    }
    // 把当前编辑器内容同步进草稿表；空草稿视为过期并移除，避免留下幽灵 scope。
    void SnapshotCurrentDraft(){
        var key=project?.Id??"personal";
        if(HasDraft())drafts[key]=(Text("TitleInput"),Text("ContentInput"),Text("SourceInput"),Text("ConditionsInput"),selected,origin,SelectedKind(),eventId,lastRequest);
        else drafts.Remove(key);
    }
    int PendingDraftCount(){SnapshotCurrentDraft();return drafts.Count;}
    ProjectItem? ProjectFor(string id){
        if(id=="personal")return null;
        return Find<ListBox>("Projects").Items.Cast<ProjectItem>().FirstOrDefault(x=>x.Id==id)??new ProjectItem(id,id);
    }
    void SetShellEnabled(bool enabled){Find<Border>("HeaderBar").IsEnabled=enabled;Find<Grid>("MainContent").IsEnabled=enabled;}
    void SetClosePromptButtons(bool enabled){Find<Button>("SaveAndCloseButton").IsEnabled=enabled;Find<Button>("DiscardAndCloseButton").IsEnabled=enabled;Find<Button>("CancelCloseButton").IsEnabled=enabled;}
    void ShowClosePrompt(){
        if(Find<Border>("ClosePrompt").IsVisible)return;   // 重复触发关闭不堆叠提示
        var count=PendingDraftCount();
        Find<TextBlock>("ClosePromptMessage").Text=$"有 {count} 份草稿尚未保存，包含当前页面和其他项目中未完成的编辑。你可以保存全部草稿后关闭，或取消关闭继续编辑。";
        SetClosePromptButtons(true);
        SetShellEnabled(false);
        Find<Border>("ClosePrompt").IsVisible=true;
        Dispatcher.UIThread.Post(()=>Find<Button>("CancelCloseButton").Focus(),DispatcherPriority.Input);
    }
    void HideClosePrompt(){Find<Border>("ClosePrompt").IsVisible=false;SetShellEnabled(true);}
    void RequestCancelClose(){if(busy)return;HideClosePrompt();StatusMessage("已取消关闭，所有未保存的编辑仍保留。");}
    void CancelCloseClick(object? s,RoutedEventArgs e)=>RequestCancelClose();
    void DiscardAndCloseClick(object? s,RoutedEventArgs e){
        if(busy)return;
        drafts.Clear();                    // 显式放弃，不交给隐藏的自动保存策略
        allowClose=true;
        HideClosePrompt();
        Close();
    }
    async void SaveAndCloseClick(object? s,RoutedEventArgs e)=>await Run(async()=>{
        SetClosePromptButtons(false);      // 保存期间禁用保存/放弃/取消
        try{await SaveAllPendingAsync();}
        catch{
            HideClosePrompt();
            ShowEditor(true);              // 回到失败 scope 的编辑器，草稿与冲突反馈保留
            throw;                         // 交给 Run 呈现既有 409/错误反馈，窗口保持打开
        }
        allowClose=true;
        Close();                           // 仅当全部成功后才关闭
    });
    async Task SaveAllPendingAsync(){
        SnapshotCurrentDraft();
        closeSaveNote="";
        var saved=0;
        foreach(var key in drafts.Keys.ToList()){
            if(!drafts.ContainsKey(key))continue;
            try{await SwitchScope(ProjectFor(key));await SaveCurrentAsync();saved++;}
            catch{
                closeSaveNote=saved>0?$"已保存 {saved} 份草稿；其余草稿保存失败，窗口保持打开。":"";
                throw;   // 原始失败信息保留，Run 仍能识别 409 冲突反馈
            }
        }
        if(PendingDraftCount()>0)throw new InvalidOperationException("仍有未保存的草稿，已取消关闭。");
    }
}
