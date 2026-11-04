<#
.SYNOPSIS
    hush installer for Windows (Windows PowerShell 5.1+ and PowerShell 7+).

.DESCRIPTION
    irm https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.ps1 | iex

    With options:
    & ([scriptblock]::Create((irm https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.ps1))) -NoFfmpeg

    HUSH_DOWNLOAD_BASE overrides where archives come from (a URL or a local
    directory), for testing.
#>
[CmdletBinding()]
param(
    # Release tag to install (default: latest). Env: HUSH_VERSION
    [string]$Version = $(if ($env:HUSH_VERSION) { $env:HUSH_VERSION } else { 'latest' }),
    # Install root (default: %LOCALAPPDATA%\hush). Env: HUSH_INSTALL_DIR
    [string]$Dir = $(if ($env:HUSH_INSTALL_DIR) { $env:HUSH_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'hush' }),
    # Do not download the FFmpeg sidecar.
    [switch]$NoFfmpeg,
    # Skip the sidecar if ffmpeg and ffprobe are already on PATH.
    [switch]$FfmpegFromPath,
    # Do not change the user PATH.
    [switch]$NoModifyPath,
    # Remove hush and its PATH entry.
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # much faster Invoke-WebRequest on 5.1
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = 'oele-isis-vanderbilt/hush-gst'
$BinDir = Join-Path $Dir 'bin'

function Say([string]$msg) { Write-Host "hush-install: $msg" }
function Fail([string]$msg) { throw "hush-install: error: $msg" }

function Get-UserPath { [Environment]::GetEnvironmentVariable('Path', 'User') }
function Set-UserPath([string]$value) { [Environment]::SetEnvironmentVariable('Path', $value, 'User') }
function Split-PathList([string]$p) { if ($p) { $p -split ';' | Where-Object { $_ } } else { @() } }

if ($Uninstall) {
    if (Test-Path $Dir) {
        if (-not (Test-Path (Join-Path $Dir '.hush-install'))) {
            Fail "$Dir was not created by this installer; not removing it"
        }
        Remove-Item -Recurse -Force $Dir
        Say "removed $Dir"
    } else {
        Say "$Dir does not exist"
    }
    $entries = Split-PathList (Get-UserPath)
    if ($entries -contains $BinDir) {
        Set-UserPath (($entries | Where-Object { $_ -ne $BinDir }) -join ';')
        Say "removed $BinDir from the user PATH"
    }
    return
}

# --- platform -------------------------------------------------------------------

$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'x86_64-pc-windows-msvc'; Say 'ARM64: using the x64 build (runs under emulation)' }
    default { Fail "unsupported CPU: $arch" }
}

if ($env:HUSH_DOWNLOAD_BASE) {
    $base = $env:HUSH_DOWNLOAD_BASE.TrimEnd('/', '\')
} elseif ($Version -eq 'latest') {
    $base = "https://github.com/$Repo/releases/latest/download"
} else {
    $base = "https://github.com/$Repo/releases/download/$Version"
}

function Fetch([string]$name, [string]$dest) {
    $src = "$base/$name"
    if (Test-Path -LiteralPath $base -PathType Container) {
        Copy-Item -LiteralPath (Join-Path $base $name) $dest
    } else {
        try { Invoke-WebRequest -UseBasicParsing -Uri $src -OutFile $dest }
        catch { Fail "download failed: $src ($($_.Exception.Message))" }
    }
}

# --- install ------------------------------------------------------------------

$archive = "hush-$target.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("hush-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Say "downloading $archive ($Version)"
    Fetch $archive (Join-Path $tmp $archive)
    Fetch "$archive.sha256" (Join-Path $tmp "$archive.sha256")

    $expected = ((Get-Content (Join-Path $tmp "$archive.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $archive)).Hash.ToLower()
    if ($expected -ne $actual) { Fail "checksum mismatch for $archive (expected $expected, got $actual)" }

    $x = Join-Path $tmp 'x'
    Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath $x
    $src = Get-ChildItem -Path $x -Recurse -Filter 'hush.exe' | Select-Object -First 1
    if (-not $src) { Fail 'archive does not contain hush.exe' }

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    $exe = Join-Path $BinDir 'hush.exe'
    Copy-Item -Force $src.FullName $exe
    New-Item -ItemType File -Force -Path (Join-Path $Dir '.hush-install') | Out-Null
    Say "installed $exe"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# --- ffmpeg sidecar -----------------------------------------------------------

if (-not $NoFfmpeg) {
    $onPath = (Get-Command ffmpeg -ErrorAction SilentlyContinue) -and (Get-Command ffprobe -ErrorAction SilentlyContinue)
    if ($FfmpegFromPath -and $onPath) {
        Say "using ffmpeg from PATH ($((Get-Command ffmpeg).Source))"
    } else {
        & $exe ffmpeg install --dir (Join-Path $Dir 'ffmpeg')
        if ($LASTEXITCODE -ne 0) {
            Say 'warning: FFmpeg sidecar not installed. Install ffmpeg yourself (winget install Gyan.FFmpeg),'
            Say "         or run 'hush ffmpeg install' later."
        }
    }
}

# --- PATH -----------------------------------------------------------------------

$entries = Split-PathList (Get-UserPath)
$onUserPath = $entries -contains $BinDir
if (-not $NoModifyPath -and -not $onUserPath) {
    Set-UserPath ((@($entries) + $BinDir) -join ';')
    Say "added $BinDir to the user PATH"
}
if (-not ((Split-PathList $env:Path) -contains $BinDir)) {
    $env:Path = "$BinDir;$env:Path"   # this session too
}

Write-Host ''
& $exe doctor
Write-Host ''
Say 'done. Open a new terminal, then run: hush --help'
