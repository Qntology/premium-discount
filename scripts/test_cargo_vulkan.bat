@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_vulkan.bat"
if errorlevel 1 exit /b 1

if not defined PD_DEVICE set "PD_DEVICE=vulkan"
cd /d "%~dp0.."
echo [TEST] cargo test -p premium-discount-app (Vulkan / PD_DEVICE=%PD_DEVICE%)...
cargo test -p premium-discount-app --features vulkan %*
exit /b %errorlevel%
