param(
    [Parameter(Mandatory=$true)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$Version = $Version -replace '^v', ''

if ($Version -notmatch '^\d+\.\d+\.\d+') {
    throw "Invalid version format: $Version. Expected: X.Y.Z (e.g., 0.1.0)"
}

Write-Host "Injecting version $Version into 3 files..."

$repoRoot = $PWD.Path

$files = @(
    @{ Path = "package.json";              Pattern = '"version"\s*:\s*"[^"]*"';     Replacement = '"' + "version" + '": "' + $Version + '"' },
    @{ Path = "src-tauri/Cargo.toml";      Pattern = '(?m)^version\s*=\s*"[^"]*"';  Replacement = 'version = "' + $Version + '"' },
    @{ Path = "src-tauri/tauri.conf.json"; Pattern = '"version"\s*:\s*"[^"]*"';     Replacement = '"' + "version" + '": "' + $Version + '"' }
)

foreach ($file in $files) {
    $fullPath = Join-Path $repoRoot $file.Path
    $content = [System.IO.File]::ReadAllText($fullPath)

    $match = [regex]::Match($content, $file.Pattern)
    if (-not $match.Success) {
        throw "No version field found in $($file.Path)"
    }

    $content = [regex]::Replace($content, $file.Pattern, $file.Replacement)
    [System.IO.File]::WriteAllText($fullPath, $content)
    Write-Host "  Updated $($file.Path)"
}

Write-Host "Done. All 3 files updated to version $Version"
