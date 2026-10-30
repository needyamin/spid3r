@echo off
cd /d "%~dp0..\..\.."
echo ========================================================
echo   Launching Spid3r Native GUI App (DataLab Studio)
echo ========================================================
.\target\debug\spid3r.exe gui examples\gui\data_studio\app.matrix --html examples\gui\data_studio\index.html
pause
