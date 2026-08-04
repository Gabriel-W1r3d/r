# Sets up the Rust/MinGW toolchain environment and builds + runs Ohm Player.
# Usage: .\run.ps1            (debug build)
#        .\run.ps1 -Release   (optimized build, small binary & low memory)
param([switch]$Release)

$env:RUSTUP_HOME = 'D:\devtools\rustup'
$env:CARGO_HOME  = 'D:\devtools\cargo'
$env:PATH        = "D:\devtools\cargo\bin;D:\devtools\winlibs\mingw64\bin;$env:PATH"
$env:CARGO_TARGET_DIR = 'D:\devtools\target-ohm'
# Use Rust's bundled MinGW runtime for linking (external gcc only compiles SQLite).
$env:RUSTFLAGS = '-C link-self-contained=yes'

Set-Location $PSScriptRoot

if ($Release) {
    cargo build --release
    if ($LASTEXITCODE -eq 0) { & "$env:CARGO_TARGET_DIR\release\ohm_player.exe" }
} else {
    cargo build
    if ($LASTEXITCODE -eq 0) { & "$env:CARGO_TARGET_DIR\debug\ohm_player.exe" }
}
