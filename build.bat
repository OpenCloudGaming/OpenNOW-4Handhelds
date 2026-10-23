@echo off
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" %*
if errorlevel 1 (
    echo Build failed.
    pause
    exit /b 1
)
