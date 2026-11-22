@echo off
setlocal EnableExtensions
pushd "%~dp0" || exit /b 1

if not "%~1"=="" if not exist ".cargo\config.local.toml" (
    echo 缺少 .cargo\config.local.toml：复制 .cargo\config.local.example.toml 为该文件，并按本机路径填写 remap 映射。
    popd
    exit /b 1
)

if /i "%~1"=="host" goto :host
if /i "%~1"=="host-femtovg" goto :host_femtovg
if /i "%~1"=="plugins" goto :plugins
if /i "%~1"=="plugin" goto :plugin
if /i "%~1"=="all" goto :all
if /i "%~1"=="test-host" goto :test_host
if /i "%~1"=="test-plugins" goto :test_plugins
if /i "%~1"=="test-plugin" goto :test_plugin
if /i "%~1"=="test-all" goto :test_all
if /i "%~1"=="check-docs" goto :check_docs
goto :usage

:host
call :ensure_skia
if errorlevel 1 goto :result
cargo build --release --locked -p agent-session-manager
goto :result

:host_femtovg
cargo build --release --locked -p agent-session-manager --no-default-features
goto :result

:plugins
cargo build --release --locked --workspace --exclude agent-session-manager
goto :result

:plugin
if "%~2"=="" goto :usage
cargo build --release --locked -p agent-plugin-%~2
goto :result

:all
call :ensure_skia
if errorlevel 1 goto :result
cargo build --release --locked --workspace
goto :result

:test_host
call :ensure_skia
if errorlevel 1 goto :result
cargo test --locked -p agent-session-manager
goto :result

:test_plugins
cargo test --locked --workspace --exclude agent-session-manager
goto :result

:test_plugin
if "%~2"=="" goto :usage
cargo test --locked -p agent-plugin-%~2
goto :result

:test_all
call :ensure_skia
if errorlevel 1 goto :result
cargo test --locked --workspace
goto :result

:check_docs
set "RUSTDOCFLAGS=-D warnings"
cargo doc --locked -p agent-core --no-deps
goto :result

:usage
echo 用法:
echo   build.cmd host                 只编译 app.exe（Skia 渲染器）
echo   build.cmd host-femtovg         编译 femtovg 版本，不需要 Skia 预编译库
echo   build.cmd plugins              只编译九个插件
echo   build.cmd plugin ^<id^>         只编译指定插件，例如 qoder
echo   build.cmd all                  编译 app.exe 与全部插件
echo   build.cmd test-host            只测试宿主
echo   build.cmd test-plugins         只测试插件
echo   build.cmd test-plugin ^<id^>    只测试指定插件
echo   build.cmd test-all             测试全部
echo   build.cmd check-docs           检查 agent-core 文档与链接
echo.
echo 等价的 cargo 参数:
echo   cargo build --release -p agent-session-manager
echo   cargo build --release --workspace --exclude agent-session-manager
echo   cargo build --release -p agent-plugin-qoder
echo   cargo build --release --workspace
echo.
echo host/all/test-host/test-all 缺少 third_party\skia 时自动下载；cargo build/run 不自动下载。
popd
exit /b 2

:ensure_skia
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0fetch-skia.ps1"
exit /b %ERRORLEVEL%

:result
set "RC=%ERRORLEVEL%"
popd
if not "%RC%"=="0" (
    echo build failed
    exit /b %RC%
)
echo ok
exit /b 0