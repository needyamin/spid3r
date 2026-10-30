@echo off
cd /d "%~dp0..\..\.."
echo ========================================================
echo   Launching Spid3r Web Application Server (CloudOps)
echo   Accessible at http://127.0.0.1:8090/
echo ========================================================
.\target\debug\spid3r.exe run --engine tree examples\web\cloudops_hub\server.matrix
pause
