@echo off
setlocal
cd /d "%~dp0"
if exist "%~dp0ohm_player.exe" (
    "%~dp0ohm_player.exe"
    exit /b %errorlevel%
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run.ps1"
