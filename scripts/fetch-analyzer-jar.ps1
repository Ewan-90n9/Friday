$ErrorActionPreference = "Stop"
$manifest = Get-Content (Join-Path $PSScriptRoot "vendor-versions.json") -Raw | ConvertFrom-Json
$dep = $manifest.analyzer
# 产物由 .github/workflows/analyzer-jar.yml 从上游 pinned tag + 补丁构建、发布到
# 本仓库 Releases（issue #9 retained 排序修复随补丁携带）
$url = "https://github.com/$($dep.release_repo)/releases/download/$($dep.tag)/$($dep.asset)"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\analyzer"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir $dep.asset
if (Test-Path $dest) {
    Write-Host "JAR already present: $dest"
    exit 0
}
Write-Host "Downloading $url"
$tmp = "$dest.downloading"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
$actual = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLower()
if ($null -eq $dep.sha256) {
    Write-Warning "vendor-versions.json 的 analyzer.sha256 未固定（analyzer-jar.yml 首次发布后回填）。本次跳过校验，实际哈希：$actual"
} elseif ($actual -ne $dep.sha256) {
    Remove-Item $tmp -Force
    throw "SHA256 mismatch for $($dep.asset): expected $($dep.sha256), got $actual"
}
Move-Item $tmp $dest
Write-Host "Downloaded: $dest ($((Get-Item $dest).Length) bytes)"
