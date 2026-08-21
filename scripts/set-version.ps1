param(
    [Parameter(Mandatory=$true)]
    [string]$Version
)

$Version = $Version -replace '^v', ''

if ($Version -notmatch '^\d+\.\d+\.\d+') {
    Write-Error "Invalid version format: $Version. Expected: X.Y.Z (e.g., 0.1.0)"
    exit 1
}

Write-Host "Injecting version $Version into 3 files..."

$packageJson = Get-Content "package.json" -Raw
$packageJson = $packageJson -replace '"version"\s*:\s*"[^"]*"', "`"version`": `"$Version`""
Set-Content -Path "package.json" -Value $packageJson -NoNewline
Write-Host "  Updated package.json"

$cargoToml = Get-Content "src-tauri/Cargo.toml" -Raw
$cargoToml = $cargoToml -replace '(?m)^version\s*=\s*"[^"]*"', "version = `"$Version`""
Set-Content -Path "src-tauri/Cargo.toml" -Value $cargoToml -NoNewline
Write-Host "  Updated src-tauri/Cargo.toml"

$tauriConf = Get-Content "src-tauri/tauri.conf.json" -Raw
$tauriConf = $tauriConf -replace '"version"\s*:\s*"[^"]*"', "`"version`": `"$Version`""
Set-Content -Path "src-tauri/tauri.conf.json" -Value $tauriConf -NoNewline
Write-Host "  Updated src-tauri/tauri.conf.json"

Write-Host "Done. All 3 files updated to version $Version"
