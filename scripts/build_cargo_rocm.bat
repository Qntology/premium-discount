@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_rocm.bat"
if errorlevel 1 exit /b 1

cd /d "%~dp0..\apps\desktop"

echo [BUILD] Building premium.discount Tauri application (ROCm / Release)...
cargo tauri build %* -- --features rocm
exit /b %errorlevel%
