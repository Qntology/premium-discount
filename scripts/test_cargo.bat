@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_cuda.bat"
if errorlevel 1 exit /b 1

if not defined PD_DEVICE set "PD_DEVICE=cuda"
cd /d "%~dp0.."
echo [TEST] cargo test -p premium-discount-app (CUDA / PD_DEVICE=%PD_DEVICE%)...
cargo test -p premium-discount-app --features cuda %*
exit /b %errorlevel%
