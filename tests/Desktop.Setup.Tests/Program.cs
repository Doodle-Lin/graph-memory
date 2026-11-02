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

    // Legacy migration must retain non-managed settings and extra environment variables.
    var jsonLegacy=Path.Combine(folder,"legacy-retain.json");
    File.WriteAllText(jsonLegacy,"""{"mcpServers":{"graph-memory":{"command":"mcp_server.exe","args":["--legacy"],"label":"keep-me","env":{"GM_API_URL":"http://old","KEEP_ME":"yes"}}}}""");
    AgentSetupService.Apply(Inspect(jsonLegacy,AgentFormat.ClaudeJson));
    var jv=JsonNode.Parse(File.ReadAllText(jsonLegacy))!["mcpServers"]!["graph_memory_v1"]!;
    Check(jv["label"]!.GetValue<string>()=="keep-me","legacy JSON property lost");
    Check(jv["env"]!["KEEP_ME"]!.GetValue<string>()=="yes" && jv["env"]!["GM_API_URL"]==null,"legacy JSON env lost");
    Check(jv["command"]!.GetValue<string>()==Path.GetFullPath(command) && jv["args"]!.AsArray().Count==0,"legacy JSON command not replaced");

    var tomlLegacy=Path.Combine(folder,"legacy-retain.toml");
    File.WriteAllText(tomlLegacy,"""
    [mcp_servers.graph-memory]
    command = 'mcp_server.exe'
    label = 'keep-me'
    [mcp_servers.graph-memory.env]
    GM_API_URL = 'http://old'
    KEEP_ME = 'yes'
    """);
    AgentSetupService.Apply(Inspect(tomlLegacy,AgentFormat.CodexToml));
    var tv=(Tomlyn.Model.TomlTable)((Tomlyn.Model.TomlTable)Toml.ToModel(File.ReadAllText(tomlLegacy))["mcp_servers"])["graph_memory_v1"];
    Check((string)tv["label"]=="keep-me","legacy TOML property lost");
    var tenv=(Tomlyn.Model.TomlTable)tv["env"];
    Check((string)tenv["KEEP_ME"]=="yes" && !tenv.ContainsKey("GM_API_URL"),"legacy TOML env lost");

    YamlMappingNode ParseYaml(string s){var ys=new YamlStream();ys.Load(new StringReader(s));return (YamlMappingNode)ys.Documents[0].RootNode;}
    var yamlLegacy=Path.Combine(folder,"legacy-retain.yaml");
    File.WriteAllText(yamlLegacy,"""
    mcp_servers:
      graph-memory:
        command: mcp_server.exe
        label: keep-me
        env:
          GM_API_URL: 'http://old'
          KEEP_ME: 'yes'
    """);
    AgentSetupService.Apply(Inspect(yamlLegacy,AgentFormat.HermesYaml));
    var yv=(YamlMappingNode)((YamlMappingNode)ParseYaml(File.ReadAllText(yamlLegacy)).Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("graph_memory_v1")];
    Check(((YamlScalarNode)yv.Children[new YamlScalarNode("label")]).Value=="keep-me","legacy YAML property lost");
    var yenv=(YamlMappingNode)yv.Children[new YamlScalarNode("env")];
    Check(((YamlScalarNode)yenv.Children[new YamlScalarNode("KEEP_ME")]).Value=="yes" && !yenv.Children.ContainsKey(new YamlScalarNode("GM_API_URL")),"legacy YAML env lost");

    // Unsafe legacy tool policies must be refused without touching the file.
    void RejectUnchanged(string path,AgentFormat format,string message) {
        var before=File.ReadAllBytes(path);Reject(()=>Inspect(path,format),message);Check(File.ReadAllBytes(path).SequenceEqual(before),message+" (bytes changed)");
    }
    var rl=Path.Combine(folder,"legacy-restrict.json");
    File.WriteAllText(rl,"""{"mcpServers":{"graph-memory":{"command":"mcp_server.exe","disabled_tools":["memory_write"]}}}""");
    RejectUnchanged(rl,AgentFormat.ClaudeJson,"restrictive legacy JSON accepted");
    File.WriteAllText(rl,"""{"mcpServers":{"graph-memory":{"command":"mcp_server.exe","enabled_tools":[]}}}""");
    RejectUnchanged(rl,AgentFormat.ClaudeJson,"empty legacy allowlist accepted");
    File.WriteAllText(rl,"""{"mcpServers":{"graph-memory":{"command":"mcp_server.exe","tool_policy":"x"}}}""");
    RejectUnchanged(rl,AgentFormat.ClaudeJson,"unknown legacy policy key accepted");
    var rt=Path.Combine(folder,"legacy-restrict.toml");
    File.WriteAllText(rt,"""
    [mcp_servers.graph-memory]
    command = 'mcp_server.exe'
    denied_tools = ['memory_write']
    """);
    RejectUnchanged(rt,AgentFormat.CodexToml,"restrictive legacy TOML accepted");
    var ry=Path.Combine(folder,"legacy-restrict.yaml");
    File.WriteAllText(ry,"""
    mcp_servers:
      graph-memory:
        command: mcp_server.exe
        allowed_tools:
          - memory_read
    """);
    RejectUnchanged(ry,AgentFormat.HermesYaml,"restrictive legacy YAML accepted");

    try { Inspect(rl,AgentFormat.ClaudeJson);throw new Exception("unsafe policy accepted"); }
    catch(InvalidOperationException e) { Check(e.Message.Contains("权限")&&!e.Message.Contains("tool_policy"),"policy error not actionable or exposes input"); }
    // Several recognized entries must never be silently combined.
    var mj=Path.Combine(folder,"multi.json");
    File.WriteAllText(mj,"""{"mcpServers":{"graph-memory":{"command":"mcp_server.exe"},"graph_memory_v1":{"command":"mcp_server.exe"}}}""");
    RejectUnchanged(mj,AgentFormat.ClaudeJson,"multiple known JSON entries accepted");
    var mt=Path.Combine(folder,"multi.toml");
    File.WriteAllText(mt,"[mcp_servers.graph-memory]\ncommand = 'mcp_server.exe'\n[mcp_servers.graph_memory_v1]\ncommand = 'mcp_server.exe'\n");
    RejectUnchanged(mt,AgentFormat.CodexToml,"multiple known TOML entries accepted");
    var my=Path.Combine(folder,"multi.yaml");
    File.WriteAllText(my,"mcp_servers:\n  graph-memory:\n    command: mcp_server.exe\n  graph_memory_v1:\n    command: mcp_server.exe\n");
    RejectUnchanged(my,AgentFormat.HermesYaml,"multiple known YAML entries accepted");

    // Current v1 restrictions must be preserved during a plain reinstall.
    var vt=Path.Combine(folder,"v1-restrict.toml");
    File.WriteAllText(vt,"""
    [mcp_servers.graph_memory_v1]
    command = 'mcp_server.exe'
    disabled_tools = ['memory_write']
    [mcp_servers.graph_memory_v1.env]
    EXTRA = 'keep'
    GM_API_URL = 'old'
    """);
    AgentSetupService.Apply(Inspect(vt,AgentFormat.CodexToml));
    var vtv=(Tomlyn.Model.TomlTable)((Tomlyn.Model.TomlTable)Toml.ToModel(File.ReadAllText(vt))["mcp_servers"])["graph_memory_v1"];
    Check(((Tomlyn.Model.TomlArray)vtv["disabled_tools"])[0]?.ToString()=="memory_write","v1 TOML restriction lost");
    Check((string)((Tomlyn.Model.TomlTable)vtv["env"])["EXTRA"]=="keep" && !((Tomlyn.Model.TomlTable)vtv["env"]).ContainsKey("GM_API_URL"),"v1 TOML env merge failed");
    var vy=Path.Combine(folder,"v1-restrict.yaml");
    File.WriteAllText(vy,"""
    mcp_servers:
      graph_memory_v1:
        command: mcp_server.exe
        disabled_tools:
          - memory_write
        env:
          EXTRA: keep
          GM_API_URL: old
    """);
    AgentSetupService.Apply(Inspect(vy,AgentFormat.HermesYaml));
    var vyv=(YamlMappingNode)((YamlMappingNode)ParseYaml(File.ReadAllText(vy)).Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("graph_memory_v1")];
    Check(((YamlScalarNode)((YamlSequenceNode)vyv.Children[new YamlScalarNode("disabled_tools")]).Children[0]).Value=="memory_write","v1 YAML restriction lost");
    var vyenv=(YamlMappingNode)vyv.Children[new YamlScalarNode("env")];
    Check(((YamlScalarNode)vyenv.Children[new YamlScalarNode("EXTRA")]).Value=="keep" && !vyenv.Children.ContainsKey(new YamlScalarNode("GM_API_URL")),"v1 YAML env merge failed");

    Console.WriteLine($"PASS {checks} setup checks: merge, legacy, preservation, migration policy, no-op, backup, restore, concurrent edits, invalid config, collisions");
} finally {
    var resolved=Path.GetFullPath(folder);
    if(!resolved.StartsWith(Path.GetFullPath(Path.GetTempPath()),StringComparison.OrdinalIgnoreCase) || !Path.GetFileName(resolved).StartsWith("gm-setup-",StringComparison.Ordinal))throw new Exception("Unexpected cleanup target");
    Directory.Delete(resolved,true);
}
