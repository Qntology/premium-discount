@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_rocm.bat"
if errorlevel 1 exit /b 1

if not defined PD_DEVICE set "PD_DEVICE=rocm"
cd /d "%~dp0.."
echo [TEST] cargo test -p premium-discount-app (ROCm / PD_DEVICE=%PD_DEVICE%)...
cargo test -p premium-discount-app --features rocm %*
exit /b %errorlevel%
