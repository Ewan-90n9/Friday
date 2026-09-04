$ErrorActionPreference = "Stop"
$manifest = Get-Content (Join-Path $PSScriptRoot "vendor-versions.json") -Raw | ConvertFrom-Json
$headers = @{ Accept = "application/vnd.github+json" }
if ($env:GH_TOKEN) { $headers.Authorization = "Bearer $env:GH_TOKEN" }
$findings = @()

# analyzer：查上游 releases/latest 与 pin 的基线 tag 比对（产物虽由本仓库
# analyzer-jar.yml 补丁构建，升级基线仍需人工评审 + 重跑 workflow）
$analyzer = $manifest.analyzer
$latestAnalyzer = Invoke-RestMethod -Uri "https://api.github.com/repos/$($analyzer.repo)/releases/latest" -Headers $headers
if ($latestAnalyzer.tag_name -ne $analyzer.upstream_base) {
    $findings += "analyzer 上游最新 release 为 $($latestAnalyzer.tag_name)（当前 pin 基线 $($analyzer.upstream_base)）：$($latestAnalyzer.html_url)"
}

# arthas：查 releases/latest 与 pin 的 tag 比对
$arthas = $manifest.arthas
$latestArthas = Invoke-RestMethod -Uri "https://api.github.com/repos/$($arthas.repo)/releases/latest" -Headers $headers
$pinnedTag = "arthas-all-$($arthas.version)"
if ($latestArthas.tag_name -ne $pinnedTag) {
    $findings += "arthas 上游最新 release 为 $($latestArthas.tag_name)（当前 pin $pinnedTag）：$($latestArthas.html_url)"
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
gh label create $label --force | Out-Null
$title = "vendored 依赖有上游更新"
$existing = gh issue list --label $label --state open --json number --jq ".[0].number"
if ($existing) {
    gh issue comment $existing --body "巡检更新（$(Get-Date -Format yyyy-MM-dd)）：`n`n$body"
    Write-Host "Updated issue #$existing"
} else {
    gh issue create --label $label --title $title --body "周期巡检发现以下 vendored 依赖落后于上游。升级需人工评审（工具面/行为可能变化）后同步更新 vendor-versions.json、Rust 常量与 workflow 里的 pinned SHA：`n`n$body"
    Write-Host "Created tracking issue"
}
