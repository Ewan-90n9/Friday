param(
    [string]$Version = "4.3.5"
)
$ErrorActionPreference = "Stop"
$url = "https://github.com/alibaba/arthas/releases/download/arthas-all-$Version/arthas-bin.zip"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\arthas"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "arthas-bin-$Version.zip"
if (Test-Path $dest) {
    Write-Host "arthas package already present: $dest"
    exit 0
}
Write-Host "Downloading $url"
$tmp = "$dest.downloading"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
Move-Item $tmp $dest
Write-Host "Downloaded: $dest ($((Get-Item $dest).Length) bytes)"
