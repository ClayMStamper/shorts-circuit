<#
.SYNOPSIS
  Puts ffmpeg.exe and ffprobe.exe where the release build bundles them.

.DESCRIPTION
  The release config (app/src-tauri/tauri.release.conf.json) ships FFmpeg next to the app as
  Tauri sidecars, which Tauri expects at app/src-tauri/binaries/<name>-<target triple>.exe.
  This script fills that folder, either from a local FFmpeg (-FromDir) or by downloading a
  pinned Gyan.dev build from its GitHub mirror and checking its SHA-256.

  The renders use libx264, so the bundled FFmpeg is a GPL build. The script also writes its
  license and a notice saying where the matching source is, which the GPL asks distributors to
  provide (see docs/human-scope.md, "Releases").

.PARAMETER Version
  Gyan.dev release, e.g. 9.0.2.

.PARAMETER Flavor
  essentials (smaller) or full.

.PARAMETER Sha256
  Expected SHA-256 of the zip. Defaults to $env:FFMPEG_SHA256, then to the checksum Gyan.dev
  publishes for the file (a second host).

.PARAMETER FromDir
  Copy ffmpeg.exe and ffprobe.exe from this folder instead of downloading (local builds).

.EXAMPLE
  ./scripts/fetch-ffmpeg.ps1
.EXAMPLE
  ./scripts/fetch-ffmpeg.ps1 -FromDir "C:\ffmpeg\bin"
#>
param(
  [string]$Version = "9.0.2",
  [ValidateSet("essentials", "full")]
  [string]$Flavor = "essentials",
  [string]$Sha256 = $env:FFMPEG_SHA256,
  [string]$FromDir,
  [string]$Target = "x86_64-pc-windows-msvc",
  [string]$OutDir = (Join-Path $PSScriptRoot "..\app\src-tauri\binaries")
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
New-Item -ItemType Directory -Force $OutDir | Out-Null

function Install-Tools([string]$BinDir, [string]$LicenseFile, [string]$Source) {
  foreach ($tool in "ffmpeg", "ffprobe") {
    $from = Join-Path $BinDir "$tool.exe"
    if (-not (Test-Path $from)) { throw "$from not found" }
    Copy-Item $from (Join-Path $OutDir "$tool-$Target.exe") -Force
  }
  $license = Join-Path $OutDir "FFMPEG-LICENSE.txt"
  if ($LicenseFile -and (Test-Path $LicenseFile)) {
    Copy-Item $LicenseFile $license -Force
  } else {
    Set-Content -Encoding utf8 $license @"
FFmpeg is licensed under the GNU General Public License, version 3 or later, when built with
GPL components such as libx264 (as the bundled build is).
Full text: https://www.gnu.org/licenses/gpl-3.0.txt
"@
  }
  $versionLine = (& (Join-Path $BinDir "ffmpeg.exe") -hide_banner -version | Select-Object -First 1)
  Set-Content -Encoding utf8 (Join-Path $OutDir "FFMPEG-NOTICE.txt") @"
Shorts Circuit runs ffmpeg.exe and ffprobe.exe as separate programs. They are not part of
Shorts Circuit's own (MIT-licensed) source code.

Bundled build: $versionLine
Obtained from: $Source

FFmpeg is free software; this build is distributed under the GNU GPL (see FFMPEG-LICENSE.txt).
The corresponding source code is available from https://ffmpeg.org/download.html (release
sources at https://ffmpeg.org/releases/) and, for the exact build, from the provider above.
"@
  Write-Host "Installed $versionLine into $OutDir"
}

if ($FromDir) {
  Install-Tools $FromDir (Join-Path (Split-Path $FromDir -Parent) "LICENSE") "a local FFmpeg installation (see the build line above for its provider)"
  return
}

$zipName = "ffmpeg-$Version-${Flavor}_build.zip"
$zipUrl = "https://github.com/GyanD/codexffmpeg/releases/download/$Version/$zipName"
$work = Join-Path ([IO.Path]::GetTempPath()) ("shorts-ffmpeg-" + [guid]::NewGuid())
New-Item -ItemType Directory $work | Out-Null
try {
  $zip = Join-Path $work $zipName
  Write-Host "Downloading $zipUrl"
  Invoke-WebRequest $zipUrl -OutFile $zip -UseBasicParsing

  if (-not $Sha256) {
    $shaUrl = "https://www.gyan.dev/ffmpeg/builds/packages/$zipName.sha256"
    Write-Host "No pinned checksum; reading $shaUrl"
    $raw = (Invoke-WebRequest $shaUrl -UseBasicParsing).Content
    if ($raw -is [byte[]]) { $raw = [Text.Encoding]::ASCII.GetString($raw) }
    $Sha256 = ($raw.Trim() -split '\s+')[0]
  }
  $actual = (Get-FileHash $zip -Algorithm SHA256).Hash
  if ($actual -ne $Sha256.Trim().ToUpperInvariant()) {
    throw "Checksum mismatch for ${zipName}: expected $Sha256, got $actual"
  }
  Write-Host "SHA-256 OK: $actual (pin it with FFMPEG_SHA256 to skip the second lookup)"

  # Extract outside the repository, then copy only the two programs and the license.
  $extract = Join-Path $work "x"
  Expand-Archive $zip -DestinationPath $extract
  $root = Get-ChildItem $extract -Directory | Select-Object -First 1
  Install-Tools (Join-Path $root.FullName "bin") (Join-Path $root.FullName "LICENSE") $zipUrl
} finally {
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
