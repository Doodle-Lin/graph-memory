@echo off
title Graph Memory
cd /d "%~dp0"

echo ========================================
echo   Graph Memory
echo ========================================
echo.
echo   API:   http://127.0.0.1:9121
echo   Web:   http://127.0.0.1:9121/
echo.
echo   Press Ctrl+C to stop
echo.

python -m graph_memory.server
pause
