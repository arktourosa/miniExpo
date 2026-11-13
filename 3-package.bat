@echo off
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
if not exist logs mkdir logs
taskkill /IM miniexpo.exe /F >nul 2>&1
taskkill /IM miniExpo.exe /F >nul 2>&1
timeout /t 2 /nobreak >nul
echo Building release (static runtime)...
cargo build --release > logs\package-log.txt 2>&1
if errorlevel 1 (
  echo PACKAGE-FAILED>> logs\package-log.txt
  echo Build failed - see logs\package-log.txt
  pause
  exit /b 1
)
if exist dist\miniExpo rmdir /s /q dist\miniExpo
mkdir dist\miniExpo
copy /y target\release\miniexpo.exe dist\miniExpo\miniExpo.exe >nul
echo PACKAGE-OK>> logs\package-log.txt
echo.
echo Done: dist\miniExpo
explorer dist\miniExpo
