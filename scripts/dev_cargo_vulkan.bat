@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_vulkan.bat"
if errorlevel 1 exit /b 1

if not defined PD_DEVICE set "PD_DEVICE=vulkan"
cd /d "%~dp0..\apps\desktop"

echo [DEV] Starting premium.discount Tauri application (Vulkan / PD_DEVICE=%PD_DEVICE%)...
cargo tauri dev %* -- --features vulkan
exit /b %errorlevel%
