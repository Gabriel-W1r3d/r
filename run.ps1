# Builds and runs Ohm Player on a machine that already has Rust installed.
# Usage: .\run.ps1            (debug build)
#        .\run.ps1 -Release   (optimized build, small binary & low memory)
param([switch]$Release)

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    $candidatePaths = @(
        Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe',
        Join-Path $env:LOCALAPPDATA 'Programs\Rust\bin\cargo.exe'
    )
    foreach ($candidate in $candidatePaths) {
        if (Test-Path $candidate) {
            $cargo = Get-Item $candidate
            break
        }
    }
}

if (-not $cargo) {
    throw "Rust/Cargo not found. Install Rust from https://rustup.rs and reopen the terminal."
}

$cargoBin = Split-Path $cargo.Source
if ($cargoBin) {
    $env:PATH = "$cargoBin;$env:PATH"
}

Set-Location $PSScriptRoot

if ($Release) {
    & $cargo.Source build --release
    if ($LASTEXITCODE -eq 0) { & ".\target\release\ohm_player.exe" }
} else {
    & $cargo.Source build
    if ($LASTEXITCODE -eq 0) { & ".\target\debug\ohm_player.exe" }
}
