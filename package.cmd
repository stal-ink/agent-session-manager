@echo off
setlocal EnableExtensions
pushd "%~dp0" || exit /b 1

set "SKIP_TESTS="
if /i "%~1"=="--skip-tests" set "SKIP_TESTS=1"

set "PKG_NAME="
for /f "tokens=1,2 delims== " %%A in ('findstr /b /c:"name = " Cargo.toml') do (
    if not defined PKG_NAME set "PKG_NAME=%%B"
)
set "VERSION="
for /f "tokens=1,2 delims== " %%A in ('findstr /b /c:"version = " Cargo.toml') do (
    if not defined VERSION set "VERSION=%%B"
)
set "PKG_NAME=%PKG_NAME:"=%"
set "VERSION=%VERSION:"=%"

if not defined PKG_NAME goto :bad_manifest
if not defined VERSION goto :bad_manifest

set "ARTIFACT=%PKG_NAME%-v%VERSION%-win-x64"
set "RELEASE_EXE=target\release\%PKG_NAME%.exe"
set "DIST=dist"
set "DIST_EXE=%DIST%\%ARTIFACT%.exe"
set "DIST_ZIP=%DIST%\%ARTIFACT%.zip"

powershell -NoProfile -NonInteractive -Command "$paths = @('%CD%\%RELEASE_EXE%','%CD%\%DIST_EXE%'); if (Get-Process -Name '%PKG_NAME%','%ARTIFACT%' -ErrorAction SilentlyContinue | Where-Object { $paths -contains $_.Path }) { exit 1 }"
if errorlevel 1 goto :running

echo [0/3] prepare skia prebuilt library
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0fetch-skia.ps1"
if errorlevel 1 goto :failed

if not defined SKIP_TESTS (
    echo [1/3] cargo test --locked -p agent-session-manager
    cargo test --locked -p agent-session-manager
    if errorlevel 1 goto :failed
) else (
    echo [1/3] tests skipped
)

echo [2/3] cargo build --release --locked -p agent-session-manager
cargo build --release --locked -p agent-session-manager
if errorlevel 1 goto :failed

if not exist "%RELEASE_EXE%" goto :missing_exe
if not exist "%DIST%" mkdir "%DIST%"
set "COPIED="
for /l %%I in (1,1,10) do (
    if not defined COPIED (
        copy /y "%RELEASE_EXE%" "%DIST_EXE%" >nul 2>&1
        if not errorlevel 1 set "COPIED=1"
        if not defined COPIED ping -n 2 127.0.0.1 >nul
    )
)
if not defined COPIED goto :failed_copy

powershell -NoProfile -NonInteractive -Command "Add-Type -AssemblyName System.IO.Compression; Add-Type -AssemblyName System.IO.Compression.FileSystem; $src='%CD%\%DIST_EXE%'; $dst='%CD%\%DIST_ZIP%'; $fs=[System.IO.File]::Open($dst,[System.IO.FileMode]::Create,[System.IO.FileAccess]::ReadWrite,[System.IO.FileShare]::None); try { $zip=New-Object System.IO.Compression.ZipArchive($fs,[System.IO.Compression.ZipArchiveMode]::Create); try { [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip,$src,[System.IO.Path]::GetFileName($src),[System.IO.Compression.CompressionLevel]::Optimal) } finally { $zip.Dispose() } } finally { $fs.Dispose() }"
if errorlevel 1 goto :failed_zip
if not exist "%DIST_ZIP%" goto :failed_zip

echo [3/3] packaged
echo   %DIST_EXE%
echo   %DIST_ZIP%
for %%F in ("%DIST_EXE%" "%DIST_ZIP%") do echo   %%~nxF  %%~zF bytes
popd
exit /b 0

:bad_manifest
echo package name or version not found in Cargo.toml
goto :failed

:running
echo close %PKG_NAME%.exe or %ARTIFACT%.exe before packaging
goto :failed

:missing_exe
echo release binary not found: %RELEASE_EXE%
goto :failed

:failed_copy
echo failed to copy release binary; close any running copy and retry
goto :failed

:failed_zip
echo failed to create zip archive
goto :failed

:failed
popd
exit /b 1
