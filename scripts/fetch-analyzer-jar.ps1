$ErrorActionPreference = "Stop"
$manifest = Get-Content (Join-Path $PSScriptRoot "vendor-versions.json") -Raw | ConvertFrom-Json
$dep = $manifest.analyzer
$url = "https://github.com/$($dep.repo)/releases/download/v$($dep.version)/$($dep.asset)"
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
$hash = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLower()
if ($hash -ne $dep.sha256) {
    Remove-Item $tmp -Force
    throw "SHA256 mismatch for $($dep.asset): expected $($dep.sha256), got $hash"
}
Move-Item $tmp $dest
Write-Host "Downloaded and verified: $dest ($((Get-Item $dest).Length) bytes)"
