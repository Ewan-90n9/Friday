$ErrorActionPreference = "Stop"
$manifest = Get-Content (Join-Path $PSScriptRoot "vendor-versions.json") -Raw | ConvertFrom-Json
$dep = $manifest.jmc
$url = "https://github.com/$($dep.release_repo)/releases/download/$($dep.tag)/$($dep.asset)"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\jmc"
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
    Write-Warning "vendor-versions.json 的 jmc.sha256 未固定（jmc-jar.yml 首次发布后回填）。本次跳过校验，实际哈希：$actual"
} elseif ($actual -ne $dep.sha256) {
    Remove-Item $tmp -Force
    throw "SHA256 mismatch for $($dep.asset): expected $($dep.sha256), got $actual"
}
Move-Item $tmp $dest
Write-Host "Downloaded: $dest ($((Get-Item $dest).Length) bytes)"
