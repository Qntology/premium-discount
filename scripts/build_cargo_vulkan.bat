@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_vulkan.bat"
if errorlevel 1 exit /b 1

cd /d "%~dp0..\apps\desktop"

echo [BUILD] Building premium.discount Tauri application (Vulkan / Release)...
cargo tauri build %* -- --features vulkan
exit /b %errorlevel%
