# Builds a portable release of UI3 Native into .\dist\UI3Native (exe + FFmpeg DLLs + notices).
# Usage: powershell -ExecutionPolicy Bypass -File tools\package.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host "[1/3] cargo build --release -p ui3-native"
cargo build --release -p ui3-native
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$dist = Join-Path $root "dist\UI3Native"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force $dist | Out-Null

Write-Host "[2/3] copying files"
Copy-Item "target\release\ui3-native.exe" $dist
# Only the FFmpeg libraries the app links against.
foreach ($dll in Get-ChildItem "third_party\ffmpeg\bin\*.dll") {
    if ($dll.Name -match '^(avcodec|avformat|avutil|swresample|swscale)-\d+\.dll$') { Copy-Item $dll.FullName $dist }
}
New-Item -ItemType Directory -Force "$dist\licenses" | Out-Null
Copy-Item "third_party\ffmpeg\LICENSE.txt" "$dist\licenses\FFmpeg-LICENSE.txt"
# UI3 Native (like UI3) is LGPL-3.0, which supplements the GPL-3.0, so both texts ship.
Copy-Item "COPYING.LESSER" "$dist\licenses\LGPL-3.0.txt"
Copy-Item "COPYING" "$dist\licenses\GPL-3.0.txt"
@"
UI3 Native
==========
A native Windows client for Blue Iris, ported from UI3 (https://github.com/bp2008/ui3).

Run ui3-native.exe and log in to your Blue Iris web server (e.g. http://192.168.1.10:81).
Settings are stored in %APPDATA%\UI3Native\config\settings.json; passwords in Windows Credential Manager.

UI3 Native is licensed under the GNU Lesser General Public License v3.0, like UI3; see licenses\LGPL-3.0.txt,
which supplements the GNU General Public License v3.0 in licenses\GPL-3.0.txt.

FFmpeg (LGPL v2.1+) is used as unmodified shared libraries (avcodec, avformat, avutil, swresample, swscale)
from the BtbN FFmpeg-Builds project; see licenses\FFmpeg-LICENSE.txt. You may replace these DLLs with
compatible builds.
"@ | Set-Content "$dist\README.txt" -Encoding utf8

Write-Host "[3/3] done"
Get-ChildItem $dist | Select-Object Name, @{n = 'KB'; e = { [int]($_.Length / 1KB) } } | Format-Table -AutoSize
$size = (Get-ChildItem $dist -Recurse | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("Package size: {0:N1} MB  ->  {1}" -f $size, $dist)
