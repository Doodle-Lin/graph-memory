using GraphMemory.Desktop;
using System.Text.Json.Nodes;
using Tomlyn;
using YamlDotNet.RepresentationModel;

var folder=Path.Combine(Path.GetTempPath(),"gm-setup-"+Guid.NewGuid().ToString("N"));Directory.CreateDirectory(folder);
var command=Path.Combine(folder,"mcp_server.exe");var service=Path.Combine(folder,"graph-memory-service.exe");File.WriteAllText(command,"");File.WriteAllText(service,"");
int checks=0;
void Check(bool ok,string message){if(!ok)throw new Exception(message);checks++;}
void Reject(Action action,string message){try{action();}catch(InvalidOperationException){checks++;return;}throw new Exception(message);}
SetupPlan Inspect(string path,AgentFormat format)=>AgentSetupService.Inspect(new("test",path,format),command,service,Path.Combine(folder,"记忆 data"));
try {
    var examples=new Dictionary<AgentFormat,string>{
        [AgentFormat.ClaudeJson]="""
        {"theme":"dark","projects":{"demo":{"allowedTools":["Read"]}},"mcpServers":{"unrelated":{"command":"other.exe","env":{"SECRET":"test-secret"}},"graph-memory":{"command":"C:\\old\\mcp_server.exe","env":{"GM_API_URL":"http://127.0.0.1:9121"}}}}
        """,
        [AgentFormat.CodexToml]="""
        model = "custom"
        [features]
        enabled = false
        [mcp_servers.unrelated]
        command = 'other.exe'
        timeout = 12.5
        [mcp_servers.graph-memory]
        command = 'C:\old\mcp_server.exe'
        [mcp_servers.graph-memory.env]
        GM_API_URL = 'http://127.0.0.1:9121'
        """,
        [AgentFormat.HermesYaml]="""
        model: custom
        flag: false
        number: 12.5
        mcp_servers:
          unrelated:
            command: other.exe
            env:
              SECRET: "test-secret"
              BOOLEAN_TEXT: "false"
          graph-memory:
            command: python
            args: ["-m", "graph_memory.mcp_server"]
            env:
              PYTHONPATH: 'E:\workspace\graph-memory'
        """
    };
    foreach(var (format,input) in examples) {
        var path=Path.Combine(folder,format.ToString()+".config");File.WriteAllText(path,input);
        var original=File.ReadAllBytes(path);var plan=Inspect(path,format);
        Check(plan.Changed && plan.Replaced.SequenceEqual(new[]{"graph-memory"}),"legacy not detected");
        var receipt=AgentSetupService.Apply(plan)!;Check(File.ReadAllBytes(receipt.Backup).SequenceEqual(original),"backup differs");
        var installed=File.ReadAllText(path);Check(!Inspect(path,format).Changed,"install not idempotent: "+format);
        Check(AgentSetupService.LastReceipt(path)!=null,"restore not discoverable");
        if(format==AgentFormat.ClaudeJson) {
            var old=JsonNode.Parse(input)!;var next=JsonNode.Parse(installed)!;
            Check(JsonNode.DeepEquals(old["projects"],next["projects"]),"projects modified");
            Check(JsonNode.DeepEquals(old["mcpServers"]!["unrelated"],next["mcpServers"]!["unrelated"]),"other JSON server modified");
        } else if(format==AgentFormat.CodexToml) {
            var old=Toml.ToModel(input);var next=Toml.ToModel(installed);
            Check((string)next["model"]=="custom","model modified");
            Check(Toml.FromModel((Tomlyn.Model.TomlTable)old["features"])==Toml.FromModel((Tomlyn.Model.TomlTable)next["features"]),"features modified");
            Check(Toml.FromModel((Tomlyn.Model.TomlTable)((Tomlyn.Model.TomlTable)old["mcp_servers"])["unrelated"])==Toml.FromModel((Tomlyn.Model.TomlTable)((Tomlyn.Model.TomlTable)next["mcp_servers"])["unrelated"]),"other TOML server modified");
        } else {
            YamlMappingNode Parse(string s){var ys=new YamlStream();ys.Load(new StringReader(s));return (YamlMappingNode)ys.Documents[0].RootNode;}
            var old=Parse(input);var next=Parse(installed);
            Check(old.Children[new YamlScalarNode("flag")].Equals(next.Children[new YamlScalarNode("flag")]),"YAML bool modified");
            Check(((YamlMappingNode)old.Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("unrelated")].Equals(((YamlMappingNode)next.Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("unrelated")]),"other YAML server modified");
        }
        File.AppendAllText(path,"\n ");Reject(()=>AgentSetupService.Restore(receipt),"overwrote newer config");
        File.WriteAllText(path,installed);AgentSetupService.Restore(receipt);Check(File.ReadAllBytes(path).SequenceEqual(original),"restore differs");
        plan=Inspect(path,format);File.AppendAllText(path,"\n ");Reject(()=>AgentSetupService.Apply(plan),"stale plan applied");
    }
    foreach(var format in Enum.GetValues<AgentFormat>()) {
        var path=Path.Combine(folder,"new-"+format);var receipt=AgentSetupService.Apply(Inspect(path,format))!;
        Check(!Inspect(path,format).Changed,"new config not idempotent");AgentSetupService.Restore(receipt);Check(!File.Exists(path),"new config not removed on undo");
    }
    var bad=Path.Combine(folder,"bad.json");File.WriteAllText(bad,"{secret: confidential");Reject(()=>Inspect(bad,AgentFormat.ClaudeJson),"invalid accepted");Check(File.ReadAllText(bad)=="{secret: confidential","invalid file modified");
    File.WriteAllText(bad,"{\"mcpServers\":{\"graph_memory_v1\":{\"command\":\"unrelated.exe\"}}}");Reject(()=>Inspect(bad,AgentFormat.ClaudeJson),"name collision accepted");
    var aliases=Path.Combine(folder,"aliases.yaml");File.WriteAllText(aliases,"defaults: &defaults {enabled: true}\nmcp_servers: *defaults\n");Reject(()=>Inspect(aliases,AgentFormat.HermesYaml),"shared YAML alias accepted");
    File.WriteAllText(bad,"{\"mcpServers\":{\"graph_memory_v1\":{\"command\":\"mcp_server.exe\",\"disabled_tools\":[\"memory_write\"],\"env\":{\"EXTRA\":\"keep\",\"GM_API_URL\":\"old\"}}}}");
    AgentSetupService.Apply(Inspect(bad,AgentFormat.ClaudeJson));var result=JsonNode.Parse(File.ReadAllText(bad))!["mcpServers"]!["graph_memory_v1"]!;
    Check(result["disabled_tools"]![0]!.GetValue<string>()=="memory_write","host tool restriction lost");Check(result["env"]!["EXTRA"]!.GetValue<string>()=="keep" && result["env"]!["GM_API_URL"]==null,"environment merge failed");
    Console.WriteLine($"PASS {checks} setup checks: merge, legacy, preservation, no-op, backup, restore, concurrent edits, invalid config, collisions");
} finally {
    var resolved=Path.GetFullPath(folder);
    if(!resolved.StartsWith(Path.GetFullPath(Path.GetTempPath()),StringComparison.OrdinalIgnoreCase) || !Path.GetFileName(resolved).StartsWith("gm-setup-",StringComparison.Ordinal))throw new Exception("Unexpected cleanup target");
    Directory.Delete(resolved,true);
}
