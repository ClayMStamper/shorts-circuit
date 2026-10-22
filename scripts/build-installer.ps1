<#
.SYNOPSIS
  Builds the Windows installer in one step.

.DESCRIPTION
  Fetches FFmpeg for the bundle if app/src-tauri/binaries doesn't have it yet (see
  fetch-ffmpeg.ps1), installs the frontend packages, runs the release build with
  tauri.release.conf.json, and prints where the installer landed.

  The installer is built from the working copy as it is, uncommitted changes included; the
  script says which branch that is and whether it has local changes.

.PARAMETER FfmpegFromDir
  Copy ffmpeg.exe and ffprobe.exe from this folder instead of downloading them (only used when
  the bundle's FFmpeg is missing).

.EXAMPLE
  ./scripts/build-installer.ps1
.EXAMPLE
  ./scripts/build-installer.ps1 -FfmpegFromDir "C:\ffmpeg\bin"
#>
param(
  [string]$FfmpegFromDir,
  [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$app = Join-Path $repo "app"
$binaries = Join-Path $app "src-tauri\binaries"

# Native programs don't stop the script on failure by themselves.
function Invoke-Step([string]$Label, [scriptblock]$Run) {
  Write-Host "`n== $Label" -ForegroundColor Cyan
  & $Run
  if ($LASTEXITCODE -ne 0) { throw "$Label failed (exit code $LASTEXITCODE)" }
}

foreach ($tool in "pnpm", "cargo") {
  if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "$tool is not on PATH" }
}

$branch = (git -C $repo branch --show-current)
$dirty = @(git -C $repo status --porcelain).Count
Write-Host "Building from branch '$branch'$(if ($dirty) { " with $dirty uncommitted change(s)" })"

$needed = "ffmpeg-$Target.exe", "ffprobe-$Target.exe", "FFMPEG-LICENSE.txt", "FFMPEG-NOTICE.txt"
$missing = @($needed | Where-Object { -not (Test-Path (Join-Path $binaries $_)) })
if ($missing.Count) {
  Invoke-Step "Fetching FFmpeg (missing: $($missing -join ', '))" {
    $global:LASTEXITCODE = 0
    if ($FfmpegFromDir) {
      & (Join-Path $PSScriptRoot "fetch-ffmpeg.ps1") -FromDir $FfmpegFromDir -Target $Target
    } else {
      & (Join-Path $PSScriptRoot "fetch-ffmpeg.ps1") -Target $Target
    }
  }
} else {
  Write-Host "FFmpeg for the bundle is already in $binaries"
}

Push-Location $app
try {
  # CI=true lets pnpm replace a node_modules folder without asking (no prompt in scripts).
  $env:CI = "true"
  Invoke-Step "Installing frontend packages" { pnpm install --frozen-lockfile }
  Remove-Item Env:CI
  Invoke-Step "Building the release installer (several minutes on a fresh build)" {
    pnpm tauri build --config src-tauri/tauri.release.conf.json
  }
} finally {
  Remove-Item Env:CI -ErrorAction SilentlyContinue
  Pop-Location
}

$installer = Get-ChildItem (Join-Path $app "src-tauri\target\release\bundle\nsis") -Filter "*-setup.exe" |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) { throw "The build finished but no installer was found" }
Write-Host "`nInstaller ($([math]::Round($installer.Length / 1MB)) MB):" -ForegroundColor Green
Write-Host $installer.FullName
Write-Host "Close Shorts Circuit before running it; your library and settings are kept."
