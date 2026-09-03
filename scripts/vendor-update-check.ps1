$ErrorActionPreference = "Stop"
$manifest = Get-Content (Join-Path $PSScriptRoot "vendor-versions.json") -Raw | ConvertFrom-Json
$headers = @{ Accept = "application/vnd.github+json" }
$findings = @()

# analyzer / arthas：查 releases/latest 与 pin 的 tag 比对
foreach ($name in @("analyzer", "arthas")) {
    $dep = $manifest.$name
    $latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$($dep.repo)/releases/latest" -Headers $headers
    $pinnedTag = if ($name -eq "analyzer") { "v$($dep.version)" } else { "arthas-all-$($dep.version)" }
    if ($latest.tag_name -ne $pinnedTag) {
        $findings += "$name 上游最新 release 为 $($latest.tag_name)（当前 pin $pinnedTag）：$($latest.html_url)"
    }
}

# jmc：查上游 master HEAD 与 pin 的 SHA 比对
$jmc = $manifest.jmc
$head = Invoke-RestMethod -Uri "https://api.github.com/repos/$($jmc.repo)/commits/master" -Headers $headers
if ($head.sha -ne $jmc.upstream_sha) {
    $msg = ($head.commit.message -split "`n")[0]
    $findings += "jmc 上游 master HEAD 已到 $($head.sha.Substring(0, 8))（当前 pin $($jmc.upstream_sha.Substring(0, 8))）：$msg"
}

if ($findings.Count -eq 0) {
    Write-Host "All vendored deps up to date."
    exit 0
}

$body = ($findings | ForEach-Object { "- $_" }) -join "`n"
Write-Host "Outdated vendored deps:`n$body"

# 幂等开/更新 tracking issue（gh CLI 为 GitHub runner 内置）
$label = "vendor-update"
gh label create $label --force 2>$null | Out-Null
$title = "vendored 依赖有上游更新"
$existing = gh issue list --label $label --state open --json number --jq ".[0].number" 2>$null
if ($existing) {
    gh issue comment $existing --body "巡检更新（$(Get-Date -Format yyyy-MM-dd)）：`n`n$body"
    Write-Host "Updated issue #$existing"
} else {
    gh issue create --label $label --title $title --body "周期巡检发现以下 vendored 依赖落后于上游。升级需人工评审（工具面/行为可能变化）后同步更新 vendor-versions.json、Rust 常量与 workflow 里的 pinned SHA：`n`n$body"
    Write-Host "Created tracking issue"
}
