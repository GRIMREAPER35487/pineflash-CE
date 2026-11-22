# Downloads the tools the Windows installer bundles into tools\win (git-ignored):
#   dfu-util.exe - flashes the Pinecil V1 (the Pinecil V2 uses PineFlash CE's built-in flasher)
# The download is checked against a pinned SHA-256 before anything is extracted.
# Used by the GitHub release workflow and for local installer builds:
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-windows-tools.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'tools\win'

$version = '0.11'
$url = "https://dfu-util.sourceforge.net/releases/dfu-util-$version-binaries.tar.xz"
$sha256 = '6450de30a7dcd8d8c1273f43f0b153f054fd24d85f7f38296b1ad8edbd2ddb25'

$work = Join-Path ([IO.Path]::GetTempPath()) "pineflash-tools-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    $archive = Join-Path $work 'dfu-util.tar.xz'
    Write-Host "Downloading dfu-util $version..."
    Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
    $actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $sha256) { throw "dfu-util checksum mismatch: got $actual, expected $sha256" }

    $member = "dfu-util-$version-binaries/win64/dfu-util-static.exe"
    tar -xf $archive -C $work $member
    if ($LASTEXITCODE -ne 0) { throw "could not extract $member" }

    New-Item -ItemType Directory -Force $out | Out-Null
    Copy-Item (Join-Path $work $member) (Join-Path $out 'dfu-util.exe') -Force
    @"
dfu-util $version (static Windows build), from $url
SHA-256 of the archive: $sha256
dfu-util is free software under the GNU General Public License, version 2 or later.
Source code: https://dfu-util.sourceforge.net/ and https://sourceforge.net/p/dfu-util/dfu-util/
"@ | Set-Content -Encoding UTF8 (Join-Path $out 'dfu-util.NOTICE.txt')
    Write-Host "dfu-util.exe ready in $out"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
