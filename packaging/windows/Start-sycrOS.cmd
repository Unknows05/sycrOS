@echo off
rem Double-click this file: opens the sycrOS menu with admin rights (needed to write USB sticks).
net session >nul 2>&1
if %errorlevel% neq 0 (
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)
cd /d "%~dp0"
sycros.exe
echo.
pause
