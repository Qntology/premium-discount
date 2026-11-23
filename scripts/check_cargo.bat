@echo off
setlocal
chcp 65001 > nul
call "%~dp0_env_cuda.bat"
if errorlevel 1 exit /b 1

cd /d "%~dp0.."
echo [CHECK] cargo check (CUDA)...
cargo check --workspace --exclude onnxruntime-sys --all-targets --features cuda %*
exit /b %errorlevel%
