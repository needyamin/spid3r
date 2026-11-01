@echo off
setlocal
title Spid3r Mini E-Commerce Platform Server (SQLite 3)

echo ======================================================================
echo       Starting Spid3r Mini E-Commerce Platform Server (SQLite 3)
echo ======================================================================
echo.

cd /d "%~dp0..\..\.."

if exist "%~dp0bin\sqlite3.dll" (
    set "SPID3R_SQLITE_PATH=%~dp0bin\sqlite3.dll"
) else (
    set "SPID3R_SQLITE_PATH=%~dp0sqlite3.dll"
)

echo [INFO] Working Directory: %CD%
echo [INFO] SQLite DLL:        %SPID3R_SQLITE_PATH%
echo [INFO] Server Script:     examples\demo_apps\ecommerce\src\server.matrix
echo [INFO] Launching Spid3r HTTP Server on http://127.0.0.1:8080...
echo.

if exist "target\release\spid3r.exe" (
    target\release\spid3r.exe run examples\demo_apps\ecommerce\src\server.matrix 8080
) else if exist "target\debug\spid3r.exe" (
    target\debug\spid3r.exe run examples\demo_apps\ecommerce\src\server.matrix 8080
) else (
    echo [INFO] Building Spid3r binary...
    cargo run --release -- run examples\demo_apps\ecommerce\src\server.matrix 8080
)

pause
