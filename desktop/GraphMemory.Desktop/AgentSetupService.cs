using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Tomlyn;
using Tomlyn.Model;
using YamlDotNet.RepresentationModel;

namespace GraphMemory.Desktop;

public enum AgentFormat { ClaudeJson, CodexToml, HermesYaml }
public sealed record AgentTarget(string Name, string Path, AgentFormat Format) {
    public override string ToString() => Name;
}
public sealed record SetupPlan(string Path, byte[]? Before, string After, string[] Replaced, bool Changed);
public sealed record SetupReceipt(string Path, string Backup, bool Existed, string AfterHash);

/// <summary>Local user-scope setup. Never edits credentials or unrelated server values.</summary>
public static class AgentSetupService {
    // Only constant, application-owned messages may bypass parser-error sanitization.
    sealed class SetupPolicyException(string message) : InvalidOperationException(message);
    const string ServerName = "graph_memory_v1";
    static readonly string[] KnownNames = ["graph-memory", "graph_memory", ServerName];
    public static AgentTarget[] Targets() {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        return [
            new("Claude Code", Path.Combine(Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR") ?? home, ".claude.json"), AgentFormat.ClaudeJson),
            new("Codex", Path.Combine(Environment.GetEnvironmentVariable("CODEX_HOME") ?? Path.Combine(home,".codex"),"config.toml"), AgentFormat.CodexToml),
            new("Hermes 命令行", Path.Combine(Environment.GetEnvironmentVariable("HERMES_HOME") ?? Path.Combine(home,".hermes"),"config.yaml"), AgentFormat.HermesYaml),
            new("Hermes 桌面版", Path.Combine(local,"Hermes Agent CN Desktop","data","hermes-home","config.yaml"), AgentFormat.HermesYaml)
        ];
    }
    public static SetupPlan Inspect(AgentTarget target, string command, string service, string dataDir) {
        if (!File.Exists(command) || !File.Exists(service)) throw new InvalidOperationException("接入组件不完整，请使用完整发布目录。");
        var path = Path.GetFullPath(target.Path);
        var before = File.Exists(path) ? File.ReadAllBytes(path) : null;
        var text = before == null ? "" : new UTF8Encoding(false,true).GetString(before).TrimStart('\uFEFF');
        var desired = new JsonObject {
            ["command"]=Path.GetFullPath(command), ["args"]=new JsonArray(),
            ["env"]=new JsonObject { ["GM_MEMORY_V1"]="1",["GM_SERVICE_DIR"]=Path.GetFullPath(dataDir.Replace("\\\\?\\","")),["GM_SERVICE_EXE"]=Path.GetFullPath(service) }
        };
        var replaced = new List<string>(); bool changed = false;
        void Update(Dictionary<string,JsonNode?> entries) {
            var recognized=KnownNames.Where(entries.ContainsKey).ToArray();
            // Combining several recognized entries would silently merge unrelated policies.
            if(recognized.Length>1)
                throw new SetupPolicyException("检测到多个记忆配置入口，未自动迁移。请在宿主配置中核对并保留一个入口后重试。");
            JsonObject? legacy=null;
            if(recognized.Length==1) {
                var name=recognized[0];
                var old=entries[name] as JsonObject ?? throw new InvalidOperationException("记忆配置结构异常，请先检查宿主配置。");
                var executable=old["command"]?.GetValue<string>() ?? "";
                var args=old["args"]?.ToJsonString() ?? "";
                if (!IsGraphMemory(executable,args))
                    throw new InvalidOperationException($"{name} 被无法识别的服务占用，未覆盖。请检查配置路径。");
                // Validate before any alias is dropped; never translate old tool policies to v1.
                if (name!=ServerName) { EnsureLegacyMigratable(name,old); legacy=old; }
            }
            // Migrate by cloning the legacy entry (or the existing v1 entry) so that
            // unrelated properties and environment variables survive the rename.
            var merged=legacy!=null?(JsonObject)legacy.DeepClone()
                :entries.GetValueOrDefault(ServerName)?.DeepClone() as JsonObject ?? new JsonObject();
            // Retain host-side tool restrictions and unrelated environment variables.
            var environment=merged["env"]?.DeepClone() as JsonObject ?? new JsonObject();
            environment.Remove("GM_API_URL");
            foreach(var pair in desired["env"]!.AsObject()) environment[pair.Key]=pair.Value?.DeepClone();
            merged["command"]=desired["command"]!.DeepClone(); merged["args"]=new JsonArray(); merged["env"]=environment;
            if(target.Format==AgentFormat.ClaudeJson) merged["type"]="stdio";
            else merged["enabled"]=true;
            if(merged.ContainsKey("url"))throw new InvalidOperationException("同名配置同时含远程地址，未覆盖，请先核对。");
            foreach(var name in recognized.Where(x=>x!=ServerName)){entries.Remove(name);replaced.Add(name);changed=true;}
            if(!JsonNode.DeepEquals(entries.GetValueOrDefault(ServerName),merged))changed=true;
            entries[ServerName]=merged;
        }
        string after;
        try {
            if(target.Format==AgentFormat.ClaudeJson) {
                var root=string.IsNullOrWhiteSpace(text)?new JsonObject():JsonNode.Parse(text)!.AsObject();
                var servers=root["mcpServers"]?.AsObject() ?? new JsonObject();
                var entries=servers.ToDictionary(x=>x.Key,x=>x.Value?.DeepClone());Update(entries);
                root["mcpServers"]=new JsonObject(entries.ToArray());
                after=root.ToJsonString(new JsonSerializerOptions{WriteIndented=true})+"\n";
            } else if(target.Format==AgentFormat.CodexToml) {
                var root=Toml.ToModel(text);
                var servers=root.TryGetValue("mcp_servers",out var value)?(TomlTable)value:new TomlTable();
                var entries=servers.ToDictionary(x=>x.Key,x=>JsonSerializer.SerializeToNode(x.Value));Update(entries);
                foreach(var name in KnownNames) servers.Remove(name);
                servers[ServerName]=ToToml(entries[ServerName]!);
                root["mcp_servers"]=servers;after=Toml.FromModel(root);
                _=Toml.ToModel(after);
            } else {
                var parser=new YamlDotNet.Core.Parser(new StringReader(text));
                while(parser.MoveNext()) {
                    if(parser.Current is YamlDotNet.Core.Events.AnchorAlias || parser.Current is YamlDotNet.Core.Events.NodeEvent node && !node.Anchor.IsEmpty)
                        throw new InvalidOperationException("YAML 锚点与别名需要手动核对。");
                }
                var stream=new YamlStream();stream.Load(new StringReader(string.IsNullOrWhiteSpace(text)?"{}":text));
                if(stream.Documents.Count!=1 || stream.Documents[0].RootNode is not YamlMappingNode root)throw new InvalidOperationException("只支持单文档 YAML 配置。");
                var key=new YamlScalarNode("mcp_servers");
                var servers=root.Children.TryGetValue(key,out var value)?(YamlMappingNode)value:new YamlMappingNode();
                var entries=servers.Children.ToDictionary(x=>((YamlScalarNode)x.Key).Value!,x=>FromYaml(x.Value));Update(entries);
                // Preserve unrelated YAML nodes and scalar styles; replace only managed entries.
                foreach(var name in KnownNames)servers.Children.Remove(new YamlScalarNode(name));
                var managed=new YamlStream();managed.Load(new StringReader(entries[ServerName]!.ToJsonString()));
                servers.Add(ServerName,managed.Documents[0].RootNode);root.Children[key]=servers;
                using var writer=new StringWriter();stream.Save(writer,false);after=writer.ToString();
            }
        } catch(Exception e) when(e is not IOException && e is not UnauthorizedAccessException && e is not SetupPolicyException) {
            // Parser errors can contain secrets from the input; never surface their text.
            throw new InvalidOperationException("配置无法安全合并：格式错误、同名入口不匹配或结构不受支持。原文件未改动。");
        }
        return new(path,before,after,replaced.ToArray(),changed);
    }
    static bool IsGraphMemory(string executable,string args) =>
        Path.GetFileName(executable).Equals("mcp_server.exe",StringComparison.OrdinalIgnoreCase)
        || Path.GetFileName(executable).Equals("mcp_server",StringComparison.OrdinalIgnoreCase)
        || args.Contains("graph_memory.mcp_server",StringComparison.Ordinal);
    static readonly string[] LegacyAllowlistKeys=["enabled_tools","allowed_tools","include_tools"];
    static readonly string[] LegacyRestrictionKeys=["enabled_tools","disabled_tools","allowed_tools","denied_tools","tools","include_tools","exclude_tools"];
    /// <summary>Old tool policies cannot be translated to the v1 names; refuse instead of widening access.</summary>
    static void EnsureLegacyMigratable(string name,JsonObject legacy) {
        foreach(var key in LegacyAllowlistKeys)
            if(legacy.TryGetPropertyValue(key,out var value)&&value is not null)
                throw new SetupPolicyException("旧入口含工具限制，不能自动换成新版工具。请手动核对权限并迁移，原配置未改动。");
        foreach(var key in LegacyRestrictionKeys)
            if(legacy.TryGetPropertyValue(key,out var value)&&value is not null&&(value is not JsonArray array||array.Count>0))
                throw new SetupPolicyException("旧入口含工具限制，不能自动换成新版工具。请手动核对权限并迁移，原配置未改动。");
        foreach(var pair in legacy)
            if(pair.Key!="env"&&!LegacyRestrictionKeys.Contains(pair.Key)
               &&(pair.Key.Contains("tool",StringComparison.OrdinalIgnoreCase)
                  ||pair.Key.Contains("permission",StringComparison.OrdinalIgnoreCase)
                  ||pair.Key.Contains("approval",StringComparison.OrdinalIgnoreCase)))
                throw new SetupPolicyException("旧入口含无法识别的权限设置，请手动核对迁移，原配置未改动。");
    }
    static object ToToml(JsonNode node) => node switch {
        JsonObject obj => MakeTable(obj),
        JsonArray array => MakeArray(array),
        JsonValue val when val.TryGetValue<bool>(out var boolean) => boolean,
        JsonValue val when val.TryGetValue<long>(out var number) => number,
        JsonValue val when val.TryGetValue<double>(out var number) => number,
        _ => node.GetValue<string>()
    };
    static TomlTable MakeTable(JsonObject values) { var table=new TomlTable();foreach(var value in values)table[value.Key]=ToToml(value.Value!);return table; }
    static TomlArray MakeArray(JsonArray values) { var array=new TomlArray();foreach(var value in values)array.Add(ToToml(value!));return array; }
    static JsonNode? FromYaml(YamlNode node) => node switch {
        YamlMappingNode map => new JsonObject(map.Children.Select(x=>KeyValuePair.Create(((YamlScalarNode)x.Key).Value!,FromYaml(x.Value)))),
        YamlSequenceNode seq => new JsonArray(seq.Children.Select(FromYaml).ToArray()),
        YamlScalarNode scalar when scalar.Style==YamlDotNet.Core.ScalarStyle.Plain && bool.TryParse(scalar.Value,out var boolean) => JsonValue.Create(boolean),
        YamlScalarNode scalar => JsonValue.Create(scalar.Value),
        _ => throw new InvalidOperationException()
    };
    static string Hash(byte[] bytes)=>Convert.ToHexString(SHA256.HashData(bytes));
    static bool Matches(string path,byte[]? bytes)=>bytes==null?!File.Exists(path):File.Exists(path)&&File.ReadAllBytes(path).SequenceEqual(bytes);
    public static SetupReceipt? Apply(SetupPlan plan) {
        if(!Matches(plan.Path,plan.Before))throw new InvalidOperationException("配置刚被其他程序修改，请重新检测后接入。");
        if(!plan.Changed)return null;
        Directory.CreateDirectory(Path.GetDirectoryName(plan.Path)!);
        var backup=plan.Path+".graph-memory-"+Guid.NewGuid().ToString("N")+".bak";
        if(plan.Before!=null)File.WriteAllBytes(backup,plan.Before);
        var bytes=new UTF8Encoding(false).GetBytes(plan.After);
        var receipt=new SetupReceipt(plan.Path,backup,plan.Before!=null,Hash(bytes));
        File.WriteAllText(backup+".receipt.json",JsonSerializer.Serialize(receipt));
        if(!Matches(plan.Path,plan.Before))throw new InvalidOperationException("配置发生变化，已保留备份，未写入。");
        AtomicWrite(plan.Path,bytes);return receipt;
    }
    static void AtomicWrite(string path,byte[] bytes) {
        var temp=path+"."+Guid.NewGuid().ToString("N")+".tmp";
        try{File.WriteAllBytes(temp,bytes);File.Move(temp,path,true);}finally{if(File.Exists(temp))File.Delete(temp);}
    }
    public static SetupReceipt? LastReceipt(string path) {
        var folder=Path.GetDirectoryName(Path.GetFullPath(path))!;if(!Directory.Exists(folder))return null;
        foreach(var file in Directory.GetFiles(folder,Path.GetFileName(path)+".graph-memory-*.bak.receipt.json").OrderByDescending(File.GetLastWriteTimeUtc)) {
            try {var receipt=JsonSerializer.Deserialize<SetupReceipt>(File.ReadAllText(file));if(receipt!=null&&receipt.Path==Path.GetFullPath(path)&&File.Exists(path)&&Hash(File.ReadAllBytes(path))==receipt.AfterHash)return receipt;}catch{}
        }
        return null;
    }
    public static void Restore(SetupReceipt receipt) {
        if(!File.Exists(receipt.Path)||Hash(File.ReadAllBytes(receipt.Path))!=receipt.AfterHash)throw new InvalidOperationException("接入后配置已被修改，未覆盖后续改动。请使用备份手动核对。");
        if(receipt.Existed)AtomicWrite(receipt.Path,File.ReadAllBytes(receipt.Backup));else File.Delete(receipt.Path);
        File.Delete(receipt.Backup+".receipt.json");
    }
}
