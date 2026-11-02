using Avalonia;
namespace GraphMemory.Desktop;
internal static class Program {
    public static string[] Arguments = [];
    [STAThread] public static int Main(string[] args) {
        Arguments=args;
        if(args.Contains("--smoke-test")) return SmokeTest().GetAwaiter().GetResult();
        BuildAvaloniaApp().StartWithClassicDesktopLifetime(args); return 0;
    }
    public static AppBuilder BuildAvaloniaApp() => AppBuilder.Configure<App>().UsePlatformDetect().LogToTrace();
    static async Task<int> SmokeTest() {
        var host=new ServiceHost();
        try {
            using var client=new ServiceClient(await host.EnsureAsync(Arguments));
            var project=await client.PostAsync("api/v1/projects",new {name="Desktop smoke project"});
            var id=project["id"]!.GetValue<string>();
            await client.PostAsync("api/v1/memories",new {memory_scope="personal",event_id=Guid.NewGuid().ToString(),content="Desktop smoke experience",title="Smoke",conditions="test only"});
            var context=await client.PostAsync("api/v1/context",new {project_id=id,query="smoke"});
            if(!context["text"]!.GetValue<string>().Contains("Desktop smoke experience")) throw new Exception("Personal experience missing from context");
            Console.WriteLine("PASS desktop client: start service, create project, save personal experience, assemble context");return 0;
        } catch(Exception e) {Console.Error.WriteLine(e.Message);return 1;}
        finally{host.StopOwned();}
    }
}
