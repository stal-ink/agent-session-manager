@echo off
setlocal EnableExtensions EnableDelayedExpansion
pushd "%~dp0" || exit /b 1

rem 用法：package-plugins.cmd [id...]
rem 不传 id 时构建并打包十端；传入 id 时只处理指定插件，例如：
rem   package-plugins.cmd qoder
rem   package-plugins.cmd qoder claude
set "TARGETS=%*"
if not defined TARGETS set "TARGETS=qoder claude codex cursor dsh kimi kiro workbuddy zcode trae"

set "BUILD_ARGS="
for %%A in (%TARGETS%) do (
    call :validate %%A || goto :failed
    set "BUILD_ARGS=!BUILD_ARGS! -p agent-plugin-%%A"
)
if not defined BUILD_ARGS goto :failed

echo [1/3] cargo build --release --locked !BUILD_ARGS!
cargo build --release --locked !BUILD_ARGS!
if errorlevel 1 goto :failed

echo [2/3] package selected plugins
if not exist "dist\plugins" mkdir "dist\plugins" || goto :failed
for %%A in (%TARGETS%) do (
    call :package %%A || goto :failed
)

echo [3/3] packaged
popd
exit /b 0

:validate
for %%K in (qoder claude codex cursor dsh kimi kiro workbuddy zcode trae) do (
    if /i "%%K"=="%~1" exit /b 0
)
echo unknown plugin id: %~1
echo known: qoder claude codex cursor dsh kimi kiro workbuddy zcode trae
exit /b 1

:package
set "PKG_ID=%~1"
set "PKG_DIR=plugins\%PKG_ID%-plugin"
set "PKG_VERSION="
for /f "tokens=1,2 delims== " %%A in ('findstr /b /c:"version = " "%PKG_DIR%\Cargo.toml"') do (
    if not defined PKG_VERSION set "PKG_VERSION=%%B"
)
set "PKG_VERSION=%PKG_VERSION:"=%"
if not defined PKG_VERSION exit /b 1
set "PKG_ROOT=%CD%"
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.IO.Compression; Add-Type -AssemblyName System.IO.Compression.FileSystem; $r=$env:PKG_ROOT; $id=$env:PKG_ID; $ver=$env:PKG_VERSION; $d=Join-Path (Join-Path $r 'plugins') ($id + '-plugin'); $e=Join-Path $r ('target\release\agent-plugin-' + $id + '.exe'); $i=Join-Path $d 'icon.png'; $t=Join-Path $d 'plugin.toml.in'; foreach ($p in @($e, $i, $t)) { if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { throw ('missing packaging input: ' + $p) } }; $outDir=Join-Path $r 'dist\plugins'; New-Item -ItemType Directory -Force -Path $outDir | Out-Null; $sha=(Get-FileHash -LiteralPath $e -Algorithm SHA256).Hash.ToLowerInvariant(); $text=[System.IO.File]::ReadAllText($t).Replace('@SHA256@', $sha).Replace('@VERSION@', $ver); $out=Join-Path $outDir ($id + '-' + $ver + '.zip'); $file=[System.IO.File]::Open($out, [System.IO.FileMode]::Create, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None); try { $zip=[System.IO.Compression.ZipArchive]::new($file, [System.IO.Compression.ZipArchiveMode]::Create); try { $entry=$zip.CreateEntry('plugin.toml', [System.IO.Compression.CompressionLevel]::Optimal); $stream=$entry.Open(); $writer=[System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false)); try { $writer.Write($text) } finally { $writer.Dispose() }; [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $i, 'icon.png', [System.IO.Compression.CompressionLevel]::Optimal); [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $e, ('bin/agent-plugin-' + $id + '.exe'), [System.IO.Compression.CompressionLevel]::Optimal); $dataDir=Join-Path $d 'data'; if (Test-Path -LiteralPath $dataDir -PathType Container) { $dataRoot=(Resolve-Path -LiteralPath $dataDir).Path; Get-ChildItem -LiteralPath $dataRoot -Recurse -File | ForEach-Object { $rel=$_.FullName.Substring($dataRoot.Length + 1).Replace('\', '/'); [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $_.FullName, ('data/' + $rel), [System.IO.Compression.CompressionLevel]::Optimal) } } } finally { $zip.Dispose() } } finally { $file.Dispose() }; Write-Output ('  ' + $id + '-' + $ver + '.zip')" || exit /b 1
exit /b 0

:failed
echo plugin packaging failed
popd
exit /b 1