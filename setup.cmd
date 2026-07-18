@echo off
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
if errorlevel 1 (
  echo.
  echo Setup failed. See the error above.
  if "%~1"=="" pause
  exit /b 1
)
endlocal
