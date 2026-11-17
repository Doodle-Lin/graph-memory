# Native desktop editing and safe close

Status: source-grounded workflow, not rerun in this skill update. Earlier UIA/real-keyboard plus HTTP persistence checks passed, while capture timed out; visual acceptance remains blocked (see feature index for evidence and retained scratch). Sources: `desktop/GraphMemory.Desktop/Program.cs`, `MainWindow.axaml`, `MainWindow.axaml.cs`, `MainWindow.Closing.cs`, `ServiceHost.cs`. Use host computer-use tools and their skill instructions; inspect the live window before actions. XAML x:Name values below identify source controls, not a promise that UIA exposes them as AutomationId. Use observed accessible labels or screenshot-derived locations; no reflection, internal setters or direct database mutation. Public UIA Value actions are allowed when the observed control supports them; do not call a UIA text tree a screenshot.

## Launch and doctor

Build the three Rust binaries as in SKILL.md. Publish a candidate with .NET10 (use the installed dotnet path):

```powershell
dotnet publish desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj -r win-x64 --self-contained true -o artifacts/verification-desktop --nologo
if ($LASTEXITCODE -ne 0) { throw 'publish failed' }
$runData = Join-Path $env:TEMP ('gm-ui-verify-' + [guid]::NewGuid().ToString('N'))
$uiEvidence = Join-Path (Get-Location) ('artifacts/verification/ui-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runData,$uiEvidence | Out-Null
$ownedService = Start-Process -FilePath (Resolve-Path src-tauri/target/debug/graph-memory-service.exe) -ArgumentList @('--data-dir', ('"' + $runData + '"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput "$uiEvidence/service.out.log" -RedirectStandardError "$uiEvidence/service.err.log"
```

Poll `$runData/service.json` up to20s, failing immediately if `$ownedService.HasExited`. Parse into `$info`, then run the read-only doctor command from SKILL.md; retry readiness within that same deadline. Do not launch UI if doctor fails. Record the service PID, instance ID, source commit/dirty state and SHA256 of the service and candidate desktop in `$uiEvidence`.

Set `GM_DRAFT_DIR` to `$runData/drafts` only in the launching process, preserve its prior value and restore it immediately after starting the UI. Launch candidate with `--service-url $info.api_url`, retain its process handle `$ownedDesktop`. A visible window is required here; use `Start-Process -PassThru` for that UI, without `-WindowStyle Hidden`. No `--smoke-test`: that path creates no window.

## Drive and observable outcomes

1. Observe `Graph Memory` connected to the isolated instance. Open “数据备份与恢复” and verify displayed directory is `$runData`, then “返回记忆”. Capture `01-connected.png`. Do not proceed on another workspace or on a connection error.
2. Click “个人经验” → “记一条”. Enter a unique `UIVERIFY-<UUID>` title in “标题（可选）” (`TitleInput`) and Chinese synthetic text in “发生了什么？怎么解决的？” (`ContentInput`); click “保存”. Use “返回列表” if needed, then search the unique title via “搜索记忆” (`Query`) → “搜索”. Capture the resulting record and evidence label.
3. Read back through public `POST /api/v1/search` with UTF-8 JSON `{scope:"personal",query:"<unique title>"}`. Save response to `$uiEvidence/created.json`; require exactly one matching record with version1 and complete content. Use `Invoke-RestMethod -Method Post -ContentType 'application/json; charset=utf-8' -Body ([Text.Encoding]::UTF8.GetBytes($json))`. No direct SQL readback.
4. Open the result, change content without saving. Use Alt+F4. Expect the close confirmation with “保存全部并关闭”, “放弃草稿并关闭”, “取消关闭”; underlying editing must not remain active. Capture `03-close-prompt.png` including the full dialog.
5. Click “取消关闭”. Require edited text still present, window stays open, and public search still returns original version1. Capture `04-draft-retained.png` and save API response.
6. Alt+F4 again → “保存全部并关闭”. Require the owned desktop exits. Doctor the service again: it must remain the same instance. Search unique title: exactly one record, version2, modified content. Save `saved-after-close.json` and final identity/exit result.

Evidence must show both actual interaction and persisted result. Screenshots alone cannot prove persistence; API readback alone cannot prove layout, focus or actual keyboard handling. Include viewport/DPI in the report; one viewport does not establish multi-DPI usability.

## Cleanup and exclusions

Close only the owned window; on failure, preserve screenshot and readback before stopping its process handle. Stop only `$ownedService` and wait. Verify resolved `$runData` is the exact GUID directory created within the temporary root before recursive removal. Never remove `$uiEvidence`. Restore the caller's GM_DRAFT_DIR even on failure. Record whether cleanup completed.

Excluded: real host Agent setup writes, native backup panel interaction, arbitrary project switching, crash recovery, accessibility audit and all-DPI behavior. Existing headless editor tests cover additional state transitions but do not close these native gaps. If no native driver is available, report this feature unverified; never replace the steps with internal control setters and call it native acceptance.
