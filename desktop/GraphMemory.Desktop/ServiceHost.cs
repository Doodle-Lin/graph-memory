using System.Diagnostics;
using System.Text.Json.Nodes;
namespace GraphMemory.Desktop;
public sealed class ServiceHost {
    Process? owned;
    public async Task<string> EnsureAsync(string[] args) {
        string? Arg(string name){var i=Array.IndexOf(args,name);return i>=0&&i+1<args.Length?args[i+1]:null;}
        if(Arg("--service-url") is string url){using var c=new ServiceClient(url);await c.GetAsync("api/service");return url;}
        var dir=Path.GetFullPath(Arg("--data-dir")??Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),"GraphMemory","workspace-v1"));
        Directory.CreateDirectory(dir);
        var manifest=Path.Combine(dir,"service.json");
        async Task<string?> Discover(){
            try{
                var info=JsonNode.Parse(await File.ReadAllTextAsync(manifest))!;
                var candidate=info["api_url"]!.GetValue<string>();
                using var c=new ServiceClient(candidate);
                var live=await c.GetAsync("api/service");
                string Normalize(string path)=>Path.GetFullPath(path.Replace("\\\\?\\", "")).TrimEnd(Path.DirectorySeparatorChar);
                if(live["instance_id"]!.GetValue<string>()==info["instance_id"]!.GetValue<string>()&&live["protocol_version"]!.GetValue<int>()==1&&StringComparer.OrdinalIgnoreCase.Equals(Normalize(live["data_dir"]!.GetValue<string>()),Normalize(dir)))return candidate;
            }catch{}return null;
        }
        var existing=await Discover();if(existing!=null)return existing;
        var executable=Arg("--service-exe")??Path.Combine(AppContext.BaseDirectory,"graph-memory-service.exe");
        if(!File.Exists(executable))throw new FileNotFoundException("找不到记忆服务，请先运行构建脚本。",executable);
        var start=new ProcessStartInfo(executable){UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true};
        start.ArgumentList.Add("--data-dir");start.ArgumentList.Add(dir);
        owned=Process.Start(start)??throw new Exception("无法启动记忆服务");
        string lastError="";owned.OutputDataReceived+=(_,_)=>{};owned.ErrorDataReceived+=(_,e)=>{if(e.Data!=null)lastError=e.Data;};owned.BeginOutputReadLine();owned.BeginErrorReadLine();
        for(var i=0;i<100;i++){
            var ready=await Discover();if(ready!=null)return ready;
            if(owned.HasExited)throw new Exception("服务启动失败："+lastError);
            await Task.Delay(150);
        }
        throw new TimeoutException("服务尚未就绪，请稍后重试。");
    }
    public void StopOwned(){if(owned is {HasExited:false}){owned.Kill();owned.WaitForExit(5000);}owned?.Dispose();}
}
