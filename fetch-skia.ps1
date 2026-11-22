param(
    [string]$Url = '',
    [string]$Archive = '',
    [string]$Dir = ''
)

# Fetch the pinned Skia prebuilt package used by the default renderer.
# Only build scripts call this; plain `cargo build` / `cargo run` keep failing
# until third_party/skia is prepared.

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$tag = '0.157.0'
$key = '30669978c3f39a73e8a7-x86_64-pc-windows-msvc-d3d-ganesh-gl-jpegd-jpege-pdf'
$dir = if ($Dir) { $Dir } else { Join-Path $root 'third_party\skia' }
$tagPath = Join-Path $dir 'tag.txt'
$keyPath = Join-Path $dir 'key.txt'
$required = @('skia.lib', 'skia-bindings.lib', 'bindings.rs')

function Test-Prebuilt {
    if (-not (Test-Path -LiteralPath $tagPath -PathType Leaf)) { return $false }
    if (-not (Test-Path -LiteralPath $keyPath -PathType Leaf)) { return $false }
    if ((Get-Content -LiteralPath $tagPath -Raw).Trim() -ne $tag) { return $false }
    if ((Get-Content -LiteralPath $keyPath -Raw).Trim() -ne $key) { return $false }
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $dir $name) -PathType Leaf)) { return $false }
    }
    return $true
}

if (Test-Prebuilt) {
    Write-Output 'skia prebuilt: ready'
    exit 0
}

$work = Join-Path (Split-Path -Parent $dir) ('.skia-download-' + [guid]::NewGuid().ToString('N'))
$archivePath = Join-Path $work 'skia-binaries.tar.gz'
$extractPath = Join-Path $work 'extract'

try {
    New-Item -ItemType Directory -Force -Path $work | Out-Null

    if ($Archive) {
        Copy-Item -LiteralPath $Archive -Destination $archivePath -Force
    } else {
        if (-not $Url) { $Url = $env:SKIA_BINARIES_URL }
        if (-not $Url) {
            $Url = "https://github.com/rust-skia/skia-binaries/releases/download/$tag/skia-binaries-$key.tar.gz"
        }
        Write-Output "skia prebuilt: downloading $Url"
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $downloaded = $false
        for ($attempt = 1; $attempt -le 3; $attempt++) {
            try {
                Invoke-WebRequest -Uri $Url -OutFile $archivePath -UseBasicParsing
                $downloaded = $true
                break
            } catch {
                Write-Output "skia prebuilt: download failed ($attempt/3): $($_.Exception.Message)"
                Start-Sleep -Seconds 2
            }
        }
        if (-not $downloaded) {
            throw "failed to download skia prebuilt package: $Url"
        }
    }

    New-Item -ItemType Directory -Force -Path $extractPath | Out-Null
    & tar.exe -xzf $archivePath -C $extractPath
    if ($LASTEXITCODE -ne 0) {
        throw 'failed to extract skia prebuilt package'
    }

    $nested = Join-Path $extractPath 'skia-binaries'
    if (-not (Test-Path -LiteralPath $nested -PathType Container)) {
        throw 'unexpected skia prebuilt package layout: missing skia-binaries/'
    }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item -Path (Join-Path $nested '*') -Destination $dir -Recurse -Force

    if (-not (Test-Prebuilt)) {
        throw 'skia prebuilt package failed validation after extraction'
    }

    Write-Output 'skia prebuilt: ready'
} finally {
    if (Test-Path -LiteralPath $work) {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}
