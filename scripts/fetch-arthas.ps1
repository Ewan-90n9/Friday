$ErrorActionPreference = "Stop"
$manifest = Get-Content (Join-Path $PSScriptRoot "vendor-versions.json") -Raw | ConvertFrom-Json
$dep = $manifest.arthas
# 远程资产名（remote_asset）与本地保存名（asset）不同：上游 Release 资产固定叫
# arthas-bin.zip，本地按带版本号命名（provision/arthas.rs 依赖该名字）
$remote = if ($dep.remote_asset) { $dep.remote_asset } else { $dep.asset }
$url = "https://github.com/$($dep.repo)/releases/download/arthas-all-$($dep.version)/$remote"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\arthas"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir $dep.asset
if (Test-Path $dest) {
    Write-Host "arthas package already present: $dest"
    exit 0
}
Write-Host "Downloading $url"
$tmp = "$dest.downloading"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
$hash = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLower()
if ($hash -ne $dep.sha256) {
    Remove-Item $tmp -Force
    throw "SHA256 mismatch for $($dep.asset): expected $($dep.sha256), got $hash"
}
Move-Item $tmp $dest
Write-Host "Downloaded and verified: $dest ($((Get-Item $dest).Length) bytes)"
