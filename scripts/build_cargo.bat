@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_cuda.bat"
if errorlevel 1 exit /b 1

cd /d "%~dp0..\apps\desktop"

echo [BUILD] Building premium.discount Tauri application (CUDA / Release)...
cargo tauri build %* -- --features cuda
exit /b %errorlevel%
