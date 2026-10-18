# Builds the Spid3r Windows installer (NSIS).
#
# Usage:  powershell -ExecutionPolicy Bypass -File packaging\nsis\build.ps1
#
# Requires: a Rust toolchain, Python with Pillow (icon generation), and
# makensis.exe under packaging\nsis\nsis\nsis-3.10\ (the official portable
# NSIS zip, or change $NsisDir below).
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent (Split-Path -Parent $here)
$NsisDir = Join-Path $here "nsis\nsis-3.10"

Set-Location $root

# 1. Release build
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }

# 2. Icon + installer bitmaps from the logo
python -c @"
from PIL import Image
src = Image.open(r'src\assets\spid3r.png').convert('RGBA')
src.save(r'packaging\nsis\assets\spid3r.ico', format='ICO',
         sizes=[(256,256),(128,128),(64,64),(48,48),(32,32),(16,16)])
hdr = Image.new('RGB', (150, 57), (59, 35, 88))
logo = src.resize((46, 46), Image.LANCZOS)
hdr.paste(logo, (8, 5), logo)
hdr.save(r'packaging\nsis\assets\header.bmp', format='BMP')
wel = Image.new('RGB', (164, 314), (59, 35, 88))
big = src.resize((96, 96), Image.LANCZOS)
wel.paste(big, (34, 40), big)
wel.save(r'packaging\nsis\assets\welcome.bmp', format='BMP')
print('icons + bitmaps ok')
"@
if ($LASTEXITCODE -ne 0) { throw "icon generation failed" }

# 3. Stage the payload
New-Item -ItemType Directory -Force -Path packaging\nsis\payload | Out-Null
Copy-Item target\release\spid3r.exe packaging\nsis\payload\spid3r.exe -Force
Copy-Item README.md, doc.html packaging\nsis\payload\ -Force
Copy-Item docs packaging\nsis\payload\docs -Recurse -Force
Copy-Item examples packaging\nsis\payload\examples -Recurse -Force

# 4. Compile the installer
Set-Location packaging\nsis
& (Join-Path $NsisDir "makensis.exe") spid3r.nsi
if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
Set-Location $root
Write-Host "Installer: $root\Spid3r-Setup-0.1.0.exe"
