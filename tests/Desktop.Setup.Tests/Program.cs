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

    // YAML type corruption guard: unmanaged scalars must keep their original type and style,
    // both on legacy migration and on repeated v1 updates. Types are asserted through a schema
    // aware deserialization (not substring checks) so a flattened "42"/"null" cannot pass.
    bool IsNumber(object? value,long expected)=>value is sbyte or byte or short or ushort or int or uint or long or ulong or float or double or decimal && Convert.ToDecimal(value)==expected;
    var typed=new YamlDotNet.Serialization.DeserializerBuilder().WithAttemptingUnquotedStringTypeDeserialization().Build();
    Dictionary<object,object> DeserializeEntry(string text,string entry) {
        var document=(Dictionary<object,object>)typed.Deserialize<object>(new StringReader(text))!;
        var servers=(Dictionary<object,object>)document["mcp_servers"];
        return (Dictionary<object,object>)servers[entry];
    }
    var yamlTypes=Path.Combine(folder,"yaml-types.yaml");
    File.WriteAllText(yamlTypes,"""
    mcp_servers:
      unrelated:
        command: other.exe
        retries: 7
      graph-memory:
        command: mcp_server.exe
        timeout: 42
        auto_approve: false
        cleared: null
        quoted_number: '123'
        quoted_bool: "true"
        nested:
          depth: 2
          flags: [true, false]
        env:
          GM_API_URL: 'http://old'
          KEEP_INT: 7
          KEEP_QUOTED: '007'
    """);
    var yamlTypesOriginal=File.ReadAllBytes(yamlTypes);
    var yamlTypesPlan=Inspect(yamlTypes,AgentFormat.HermesYaml);
    Check(yamlTypesPlan.Changed,"yaml types migration not detected");
    var yamlTypesReceipt=AgentSetupService.Apply(yamlTypesPlan)!;
    Check(File.ReadAllBytes(yamlTypesReceipt.Backup).SequenceEqual(yamlTypesOriginal),"yaml types backup differs");
    var yamlTypesInstalled=File.ReadAllText(yamlTypes);
    Check(!Inspect(yamlTypes,AgentFormat.HermesYaml).Changed,"yaml types install not idempotent");
    Check(Inspect(yamlTypes,AgentFormat.HermesYaml).Changed==false && File.ReadAllText(yamlTypes)==yamlTypesInstalled,"second yaml inspect Changed not stable");
    var migrated=DeserializeEntry(yamlTypesInstalled,"graph_memory_v1");
    Check(IsNumber(migrated["timeout"],42),"legacy YAML numeric timeout corrupted");
    Check(migrated["auto_approve"] is bool approve && !approve,"legacy YAML boolean flag corrupted");
    Check(migrated["cleared"] is null,"legacy YAML explicit null corrupted");
    Check(migrated["quoted_number"] is string quotedNumber && quotedNumber=="123","legacy YAML quoted number lost");
    Check(migrated["quoted_bool"] is string quotedBool && quotedBool=="true","legacy YAML quoted bool lost");
    var nested=(Dictionary<object,object>)migrated["nested"];
    Check(IsNumber(nested["depth"],2),"legacy YAML nested number corrupted");
    var flags=(List<object>)nested["flags"];
    Check(flags.Count==2 && (bool)flags[0] && !(bool)flags[1],"legacy YAML nested array corrupted");
    var migratedEnv=(Dictionary<object,object>)migrated["env"];
    Check(IsNumber(migratedEnv["KEEP_INT"],7),"legacy YAML extra env int corrupted");
    Check(migratedEnv["KEEP_QUOTED"] is string keepQuoted && keepQuoted=="007","legacy YAML extra env quoted string lost");
    Check(!migratedEnv.ContainsKey("GM_API_URL") && migratedEnv.ContainsKey("GM_SERVICE_EXE") && migratedEnv.ContainsKey("GM_MEMORY_V1"),"legacy YAML managed env not applied");
    var migratedNodes=(YamlMappingNode)((YamlMappingNode)ParseYaml(yamlTypesInstalled).Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("graph_memory_v1")];
    Check(((YamlScalarNode)migratedNodes.Children[new YamlScalarNode("quoted_number")]).Style==YamlDotNet.Core.ScalarStyle.SingleQuoted,"legacy YAML single-quoted style lost");
    Check(((YamlScalarNode)migratedNodes.Children[new YamlScalarNode("quoted_bool")]).Style==YamlDotNet.Core.ScalarStyle.DoubleQuoted,"legacy YAML double-quoted style lost");
    var unrelated=DeserializeEntry(yamlTypesInstalled,"unrelated");
    Check(IsNumber(unrelated["retries"],7),"unrelated YAML numeric type changed");
    AgentSetupService.Restore(yamlTypesReceipt);
    Check(File.ReadAllBytes(yamlTypes).SequenceEqual(yamlTypesOriginal),"yaml types restore differs");

    var yamlV1Types=Path.Combine(folder,"yaml-v1-types.yaml");
    File.WriteAllText(yamlV1Types,"""
    mcp_servers:
      graph_memory_v1:
        command: mcp_server.exe
        timeout: 30
        cleared: null
        quoted_number: '456'
        disabled_tools:
          - memory_write
        env:
          EXTRA: keep
          GM_API_URL: old
    """);
    AgentSetupService.Apply(Inspect(yamlV1Types,AgentFormat.HermesYaml));
    var v1Installed=File.ReadAllText(yamlV1Types);
    var v1=DeserializeEntry(v1Installed,"graph_memory_v1");
    Check(IsNumber(v1["timeout"],30),"v1 YAML numeric timeout corrupted");
    Check(v1["cleared"] is null,"v1 YAML explicit null corrupted");
    Check(v1["quoted_number"] is string v1Quoted && v1Quoted=="456","v1 YAML quoted number lost");
    var v1Env=(Dictionary<object,object>)v1["env"];
    Check((string)v1Env["EXTRA"]=="keep" && !v1Env.ContainsKey("GM_API_URL") && v1Env.ContainsKey("GM_SERVICE_DIR"),"v1 YAML env merge failed");
    Check(((List<object>)v1["disabled_tools"])[0]?.ToString()=="memory_write","v1 YAML restriction lost");
    var v1Nodes=(YamlMappingNode)((YamlMappingNode)ParseYaml(v1Installed).Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("graph_memory_v1")];
    Check(((YamlScalarNode)v1Nodes.Children[new YamlScalarNode("quoted_number")]).Style==YamlDotNet.Core.ScalarStyle.SingleQuoted,"v1 YAML quoted style lost");
    Check(!Inspect(yamlV1Types,AgentFormat.HermesYaml).Changed,"v1 YAML second inspect not idempotent");
    Check(File.ReadAllText(yamlV1Types)==v1Installed,"v1 YAML second inspect rewrote file");

    Check(migratedEnv["GM_MEMORY_V1"] is string managedVersion && managedVersion=="1","managed env must be a string");
    Check(v1["enabled"] is bool enabled && enabled,"enabled must be boolean true");
    var normalizedNode=ParseYaml(v1Installed);
    var normalizedServer=(YamlMappingNode)((YamlMappingNode)normalizedNode.Children[new YamlScalarNode("mcp_servers")]).Children[new YamlScalarNode("graph_memory_v1")];
    var normalizedEnv=(YamlMappingNode)normalizedServer.Children[new YamlScalarNode("env")];
    normalizedEnv.Children[new YamlScalarNode("GM_MEMORY_V1")]=new YamlScalarNode("1"){Style=YamlDotNet.Core.ScalarStyle.Plain};
    normalizedServer.Children[new YamlScalarNode("enabled")]=new YamlScalarNode("true"){Style=YamlDotNet.Core.ScalarStyle.DoubleQuoted};
    using(var w=new StringWriter()){new YamlStream(new YamlDocument(normalizedNode)).Save(w,false);File.WriteAllText(yamlV1Types,w.ToString());}
    var normalizePlan=Inspect(yamlV1Types,AgentFormat.HermesYaml);
    Check(normalizePlan.Changed,"managed type-only change not detected");
    AgentSetupService.Apply(normalizePlan);
    var normalized=DeserializeEntry(File.ReadAllText(yamlV1Types),"graph_memory_v1");
    Check(((Dictionary<object,object>)normalized["env"])["GM_MEMORY_V1"] is string,"managed numeric env not normalized");
    Check(normalized["enabled"] is bool normalizedEnabled && normalizedEnabled,"quoted enabled not normalized");
    Check(!Inspect(yamlV1Types,AgentFormat.HermesYaml).Changed,"normalization not idempotent");
    File.WriteAllText(rl,"""{"mcpServers":{"graph-memory":{"command":"mcp_server.exe","tools":[]}}}""");
    RejectUnchanged(rl,AgentFormat.ClaudeJson,"ambiguous empty tool policy accepted");
    Console.WriteLine($"PASS {checks} setup checks: merge, legacy, preservation, migration policy, no-op, backup, restore, concurrent edits, invalid config, collisions");
} finally {
    var resolved=Path.GetFullPath(folder);
    if(!resolved.StartsWith(Path.GetFullPath(Path.GetTempPath()),StringComparison.OrdinalIgnoreCase) || !Path.GetFileName(resolved).StartsWith("gm-setup-",StringComparison.Ordinal))throw new Exception("Unexpected cleanup target");
    Directory.Delete(resolved,true);
}
