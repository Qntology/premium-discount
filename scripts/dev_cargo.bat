@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_cuda.bat"
if errorlevel 1 exit /b 1

if not defined PD_DEVICE set "PD_DEVICE=cuda"
cd /d "%~dp0..\apps\desktop"

echo [DEV] Starting premium.discount Tauri application (CUDA / PD_DEVICE=%PD_DEVICE%)...
cargo tauri dev %* -- --features cuda
exit /b %errorlevel%
