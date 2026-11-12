# Builds the Chatty desktop client for Windows x64: a portable .zip and an
# Inno Setup installer in dist\. Needs rustup, the MSVC build tools, CMake,
# NASM, and Inno Setup 6 (ISCC.exe on PATH or in its default location).
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ProjectDir = Split-Path -Parent $PSScriptRoot
$BuildRoot = Join-Path $ProjectDir 'build\windows'
$DistDir = Join-Path $ProjectDir 'dist'
$OrtVersion = '1.23.2'
$OrtName = "onnxruntime-win-x64-$OrtVersion"
$OrtSha256 = '0b38df9af21834e41e73d602d90db5cb06dbd1ca618948b8f1d66d607ac9f3cd'

$metadata = cargo metadata --manifest-path (Join-Path $ProjectDir 'Cargo.toml') --no-deps --format-version 1 | ConvertFrom-Json
$Version = ($metadata.packages | Where-Object name -eq 'chatty-gui').version
if (-not $Version) { throw 'Could not determine the chatty-gui version.' }

New-Item -ItemType Directory -Force -Path $BuildRoot, $DistDir | Out-Null
$archive = Join-Path $BuildRoot "$OrtName.zip"
function Test-OrtArchive {
    (Test-Path $archive) -and ((Get-FileHash -Algorithm SHA256 $archive).Hash -eq $OrtSha256)
}
if (-not (Test-OrtArchive)) {
    Write-Host "Downloading ONNX Runtime $OrtVersion..."
    Invoke-WebRequest -Uri "https://github.com/microsoft/onnxruntime/releases/download/v$OrtVersion/$OrtName.zip" -OutFile $archive
}
if (-not (Test-OrtArchive)) { throw "ONNX Runtime archive checksum mismatch." }
$ortDir = Join-Path $BuildRoot $OrtName
if (Test-Path $ortDir) { Remove-Item -Recurse -Force $ortDir }
Expand-Archive -Path $archive -DestinationPath $BuildRoot

$env:ORT_LIB_LOCATION = Join-Path $ortDir 'lib'
$env:ORT_PREFER_DYNAMIC_LINK = '1'
Write-Host 'Building chatty-gui...'
cargo build --manifest-path (Join-Path $ProjectDir 'Cargo.toml') --release --locked -p chatty-gui --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed.' }

$stage = Join-Path $BuildRoot 'stage'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item (Join-Path $ProjectDir 'target\x86_64-pc-windows-msvc\release\chatty-gui.exe') $stage
# The application directory is searched before System32, which can hold an
# older onnxruntime.dll.
Copy-Item (Join-Path $env:ORT_LIB_LOCATION 'onnxruntime.dll') $stage
Copy-Item (Join-Path $env:ORT_LIB_LOCATION 'onnxruntime_providers_shared.dll') $stage
Copy-Item (Join-Path $ProjectDir 'packaging\windows\chatty.ico') $stage
Copy-Item (Join-Path $ProjectDir 'LICENSE') (Join-Path $stage 'LICENSE.txt')

$zip = Join-Path $DistDir "chatty-$Version-windows-x64-portable.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Write-Host "Created $zip"

$iscc = Get-Command ISCC.exe -ErrorAction SilentlyContinue
$isccPath = if ($iscc) { $iscc.Source } else { Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe' }
if (-not (Test-Path $isccPath)) { throw 'Inno Setup 6 (ISCC.exe) is required to build the installer.' }
& $isccPath "/DAppVersion=$Version" "/DStageDir=$stage" "/O$DistDir" (Join-Path $ProjectDir 'packaging\windows\chatty.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed.' }
Write-Host "Created $(Join-Path $DistDir "chatty-$Version-windows-x64-setup.exe")"
