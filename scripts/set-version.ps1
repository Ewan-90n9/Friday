param(
    [Parameter(Mandatory=$true)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$Version = $Version -replace '^v', ''

if ($Version -notmatch '^\d+\.\d+\.\d+') {
    Write-Error "Invalid version format: $Version. Expected: X.Y.Z (e.g., 0.1.0)"
    exit 1
}

Write-Host "Injecting version $Version into 3 files..."

$packageJsonPath = "package.json"
$packageJson = [System.IO.File]::ReadAllText((Resolve-Path $packageJsonPath))
$original = $packageJson
$packageJson = $packageJson -replace '"version"\s*:\s*"[^"]*"', "`"version`": `"$Version`""
if ($packageJson -eq $original) {
    throw "No version field found in $packageJsonPath"
}
[System.IO.File]::WriteAllText((Resolve-Path $packageJsonPath), $packageJson)
Write-Host "  Updated package.json"

$cargoTomlPath = "src-tauri/Cargo.toml"
$cargoToml = [System.IO.File]::ReadAllText((Resolve-Path $cargoTomlPath))
$original = $cargoToml
$cargoToml = $cargoToml -replace '(?m)^version\s*=\s*"[^"]*"', "version = `"$Version`""
if ($cargoToml -eq $original) {
    throw "No version field found in $cargoTomlPath"
}
[System.IO.File]::WriteAllText((Resolve-Path $cargoTomlPath), $cargoToml)
Write-Host "  Updated src-tauri/Cargo.toml"

$tauriConfPath = "src-tauri/tauri.conf.json"
$tauriConf = [System.IO.File]::ReadAllText((Resolve-Path $tauriConfPath))
$original = $tauriConf
$tauriConf = $tauriConf -replace '"version"\s*:\s*"[^"]*"', "`"version`": `"$Version`""
if ($tauriConf -eq $original) {
    throw "No version field found in $tauriConfPath"
}
[System.IO.File]::WriteAllText((Resolve-Path $tauriConfPath), $tauriConf)
Write-Host "  Updated src-tauri/tauri.conf.json"

Write-Host "Done. All 3 files updated to version $Version"
