@echo off
rem Windows launcher. Run from an Administrator terminal for disk commands.
setlocal
where py >nul 2>nul && (py -3 "%~dp0sycros.py" %* & exit /b %errorlevel%)
where python >nul 2>nul && (python "%~dp0sycros.py" %* & exit /b %errorlevel%)
echo Python 3 is needed: https://www.python.org/downloads/windows/  (tick "Add to PATH")
exit /b 1
