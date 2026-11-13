@echo off
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
if not exist logs mkdir logs
taskkill /IM miniexpo.exe /F >nul 2>&1
echo Building...
cargo build --release > logs\build-log.txt 2>&1
if errorlevel 1 (
  echo BUILD-FAILED>> logs\build-log.txt
  echo Build failed - see logs\build-log.txt
  pause
  exit /b 1
)
echo BUILD-OK>> logs\build-log.txt
start "" "target\release\miniexpo.exe"
