param(
    [string]$Version = "0.2.0"
)
$ErrorActionPreference = "Stop"
$url = "https://github.com/Djaler/jvm-heap-dump-mcp/releases/download/v$Version/jvm-heap-dump-mcp-$Version-all.jar"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\analyzer"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "jvm-heap-dump-mcp-$Version-all.jar"
if (Test-Path $dest) {
    Write-Host "JAR already present: $dest"
    exit 0
}
Write-Host "Downloading $url"
$tmp = "$dest.downloading"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
Move-Item $tmp $dest
Write-Host "Downloaded: $dest ($((Get-Item $dest).Length) bytes)"
