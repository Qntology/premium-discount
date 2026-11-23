@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_rocm.bat"
if errorlevel 1 exit /b 1

if not defined PD_DEVICE set "PD_DEVICE=rocm"
cd /d "%~dp0..\apps\desktop"

echo [DEV] Starting premium.discount Tauri application (ROCm / PD_DEVICE=%PD_DEVICE%)...
cargo tauri dev %* -- --features rocm
exit /b %errorlevel%
