@echo off
if defined ATRIUM_NODE (
  setlocal
  set ELECTRON_RUN_AS_NODE=1
  "%ATRIUM_NODE%" "%~dp0atrium" %*
  exit /b %errorlevel%
)
node "%~dp0atrium" %*
