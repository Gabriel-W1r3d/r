@echo off
setlocal
set "APP_DIR=%~dp0"
set "LOCAL_CACHE=%LOCALAPPDATA%\OhmPlayer"

if exist "%APP_DIR%ohm_player.exe" (
    start "" /wait "%APP_DIR%ohm_player.exe"
    exit /b %errorlevel%
)

where powershell.exe >nul 2>nul
if errorlevel 1 (
    echo Ohm Player needs PowerShell to download the release package.
    echo Please extract the full ZIP release or install PowerShell.
    exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference = 'Stop';" ^
  "$repo = 'Gabriel-W1r3d/r';" ^
  "$cache = $env:LOCALAPPDATA + '\OhmPlayer';" ^
  "$exe = Join-Path $cache 'ohm_player.exe';" ^
  "if (-not (Test-Path $exe)) {" ^
  "  New-Item -ItemType Directory -Force $cache | Out-Null;" ^
  "  $release = Invoke-RestMethod -Headers @{ 'User-Agent' = 'OhmPlayer' } -Uri ('https://api.github.com/repos/' + $repo + '/releases/latest');" ^
  "  $asset = $release.assets | Where-Object { $_.name -like 'ohm-player-*-windows.zip' } | Select-Object -First 1;" ^
  "  if (-not $asset) { throw 'Windows release asset not found.' }" ^
  "  $zip = Join-Path $cache $asset.name;" ^
  "  Invoke-WebRequest -Headers @{ 'User-Agent' = 'OhmPlayer' } -Uri $asset.browser_download_url -OutFile $zip;" ^
  "  Expand-Archive -Path $zip -DestinationPath $cache -Force;" ^
  "}" ^
  "Start-Process -FilePath $exe -WorkingDirectory $cache"
