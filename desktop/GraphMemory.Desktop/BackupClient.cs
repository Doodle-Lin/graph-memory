using System.Diagnostics;
using System.Text;
using System.Text.Json.Nodes;

namespace GraphMemory.Desktop;

/// <summary>
/// P2-A: result of one graph-memory-backup.exe invocation. The CLI is the only writer; the
/// desktop never touches the workspace database itself.
/// </summary>
public sealed record BackupResult(int ExitCode, string StandardOutput, string StandardError) {
    public bool Ok => ExitCode == 0;
    /// <summary>Human readable failure text: exit code plus the raw stderr, never swallowed.</summary>
    public string Summary => Ok ? StandardOutput.Trim() : $"exit code {ExitCode}: {StandardError.Trim()}";
    public JsonNode? Json { get { try { return JsonNode.Parse(StandardOutput); } catch { return null; } } }
    public string Counts {
        get {
            var counts = Json?["counts"];
            if (counts == null) return "";
            return $"projects {counts["projects"]}, memories {counts["memories"]}, revisions {counts["revisions"]}, events {counts["events"]}";
        }
    }
}

/// <summary>
/// P2-A: thin wrapper around the sibling graph-memory-backup.exe. Arguments are always passed
/// through <see cref="ProcessStartInfo.ArgumentList"/> (space separated, no shell, no "--opt="
/// forms) so paths with spaces and Chinese characters cannot be reinterpreted.
/// </summary>
public static class BackupClient {
    public const string FileName = "graph-memory-backup.exe";

    /// <summary>
    /// Resolves the CLI next to the app; an explicit GM_BACKUP_EXE override keeps headless tests
    /// pointed at the real debug binary instead of a copied artifact.
    /// </summary>
    public static string ResolveExecutable() {
        var overridePath = Environment.GetEnvironmentVariable("GM_BACKUP_EXE");
        if (!string.IsNullOrWhiteSpace(overridePath)) return overridePath!;
        var beside = Path.Combine(AppContext.BaseDirectory, FileName);
        if (File.Exists(beside)) return beside;
        var assembly = Path.GetDirectoryName(typeof(BackupClient).Assembly.Location);
        if (!string.IsNullOrEmpty(assembly)) {
            var candidate = Path.Combine(assembly, FileName);
            if (File.Exists(candidate)) return candidate;
        }
        return beside;
    }

    public static Task<BackupResult> ExportAsync(string dataDir, string output) =>
        RunAsync(["export", "--data-dir", dataDir, "--output", output]);

    public static Task<BackupResult> VerifyAsync(string file) =>
        RunAsync(["verify", file]);

    public static Task<BackupResult> RestoreAsync(string input, string dataDir, bool dryRun) =>
        RunAsync(dryRun
            ? ["restore", "--input", input, "--data-dir", dataDir, "--dry-run"]
            : ["restore", "--input", input, "--data-dir", dataDir]);

    public static async Task<BackupResult> RunAsync(IReadOnlyList<string> arguments) {
        var executable = ResolveExecutable();
        if (!File.Exists(executable)) throw new FileNotFoundException("找不到 graph-memory-backup，请先运行构建脚本或打包产物。", executable);
        var start = new ProcessStartInfo(executable) {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(false),
            StandardErrorEncoding = new UTF8Encoding(false),
        };
        foreach (var argument in arguments) start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("无法启动 graph-memory-backup。");
        var stdout = process.StandardOutput.ReadToEndAsync();
        var stderr = process.StandardError.ReadToEndAsync();
        await process.WaitForExitAsync().ConfigureAwait(false);
        return new BackupResult(process.ExitCode, await stdout.ConfigureAwait(false), await stderr.ConfigureAwait(false));
    }
}
