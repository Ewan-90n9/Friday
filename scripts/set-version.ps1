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
    @{ Path = "package.json";                  Pattern = '"version"\s*:\s*"[^"]*"';                  Replacement = "`"version`": `"$Version`"" },
    @{ Path = "src-tauri/Cargo.toml";          Pattern = '(?m)^version\s*=\s*"[^"]*"';              Replacement = "version = `"$Version`"" },
    @{ Path = "src-tauri/tauri.conf.json";     Pattern = '"version"\s*:\s*"[^"]*"';                  Replacement = "`"version`": `"$Version`"" }
)

foreach ($file in $files) {
    $fullPath = Join-Path $repoRoot $file.Path
    $content = [System.IO.File]::ReadAllText($fullPath)
    $original = $content
    $content = $content -replace $file.Pattern, $file.Replacement
    if ($content -eq $original) {
        $snippet = $content.Substring(0, [Math]::Min(200, $content.Length))
        throw "No version field found in $($file.Path). File starts with: $snippet"
    }
    [System.IO.File]::WriteAllText($fullPath, $content)
    Write-Host "  Updated $($file.Path)"
}

Write-Host "Done. All 3 files updated to version $Version"
