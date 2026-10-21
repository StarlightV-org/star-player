param(
    [string]$Archive,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\dist'),
    [string]$WorkDirectory = (Join-Path $PSScriptRoot '..\target\ffmpeg-package')
)
$ErrorActionPreference = 'Stop'
function Print([string]$Message) { Write-Output $Message }

# This is a release preparation step, not part of cargo build. Only ffmpeg.exe
# and its notices are distributed. Never include ffprobe, ffplay, or this source
# archive in the portable executable or the optional package.
$sourceUrl = 'https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-full_build.7z'
$sourceHash = 'f0e46253c70dfe902bac915dfb4224f0cf1b7c6eeab9da2ccc9a5581f9a71b13'
$repo = Split-Path $PSScriptRoot -Parent
$cargo = [IO.File]::ReadAllText((Join-Path $repo 'Cargo.toml'))
$version = [regex]::Match($cargo, '(?m)^version = "([^"]+)"').Groups[1].Value
if (-not $version) { throw 'Cannot read StarPlayer version' }
New-Item -ItemType Directory -Force $OutputDirectory, $WorkDirectory | Out-Null
$OutputDirectory = (Resolve-Path $OutputDirectory).Path
$WorkDirectory = (Resolve-Path $WorkDirectory).Path
if (-not $Archive) {
    $Archive = Join-Path $WorkDirectory "ffmpeg-$sourceHash.7z"
    if (-not (Test-Path -LiteralPath $Archive)) {
        Print 'Downloading pinned FFmpeg for the optional release asset'
        $partial = "$Archive.partial"
        Invoke-WebRequest -Uri $sourceUrl -OutFile $partial -TimeoutSec 600
        if ((Get-Item -LiteralPath $partial).Length -gt 256MB) { throw 'FFmpeg source archive exceeds the size limit' }
        if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sourceHash) { throw 'FFmpeg source checksum mismatch' }
        Move-Item -LiteralPath $partial -Destination $Archive
    }
}
if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $sourceHash) { throw 'FFmpeg source checksum mismatch' }
$work = Join-Path $WorkDirectory ([guid]::NewGuid().ToString())
$extracted = Join-Path $work 'upstream'
$staging = Join-Path $work 'package'
New-Item -ItemType Directory -Force $staging | Out-Null
$sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
if ($sevenZip) { $sevenZip = $sevenZip.Source }
elseif (Test-Path -LiteralPath "$env:ProgramFiles\7-Zip\7z.exe") { $sevenZip = "$env:ProgramFiles\7-Zip\7z.exe" }
else { throw 'Install 7-Zip to prepare the optional release package' }
& $sevenZip x $Archive "-o$extracted" -y | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot extract pinned FFmpeg archive' }
$executables = @(Get-ChildItem -LiteralPath $extracted -Recurse -Filter ffmpeg.exe)
if ($executables.Count -ne 1) { throw 'Expected exactly one ffmpeg.exe' }
$ffmpeg = $executables[0].FullName
Copy-Item -LiteralPath $ffmpeg -Destination (Join-Path $staging 'ffmpeg.exe')
foreach ($name in @('LICENSE', 'README.txt')) {
    $notice = @(Get-ChildItem -LiteralPath $extracted -Recurse -File | Where-Object { $_.Name -eq $name })
    if ($notice.Count -ne 1) { throw "Expected exactly one $name notice" }
    Copy-Item -LiteralPath $notice[0].FullName -Destination (Join-Path $staging $name)
}
Copy-Item -LiteralPath (Join-Path $repo 'licenses\THIRD_PARTY_NOTICES.md') -Destination (Join-Path $staging 'THIRD_PARTY_NOTICES.md')
# Stable ZIP timestamps allow releases with identical tools/notices to reuse
# the same verified cache instead of storing another executable after updates.
Get-ChildItem -LiteralPath $staging -File | ForEach-Object { $_.LastWriteTime = [datetime]::new(2000, 1, 1) }
$decoders = & $ffmpeg -hide_banner -decoders 2>&1 | Out-String
if ($LASTEXITCODE -ne 0 -or $decoders -notmatch '(?m)^\s*V\S*\s+libdav1d\s') { throw 'The export package must include dav1d' }

$files = @(Get-ChildItem -LiteralPath $staging -File | Sort-Object Name | ForEach-Object {
    @{ name = $_.Name; size = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
})
Add-Type -AssemblyName System.IO.Compression.FileSystem
$asset = 'star-player-ffmpeg-windows-x86_64.zip'
$zip = Join-Path $work $asset
[IO.Compression.ZipFile]::CreateFromDirectory($staging, $zip, [IO.Compression.CompressionLevel]::Optimal, $false)
$manifest = @{
    version = $version
    target = 'x86_64-pc-windows-msvc'
    asset = $asset
    size = (Get-Item -LiteralPath $zip).Length
    sha256 = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    files = $files
}
$utf8 = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'export-package.json'), ($manifest | ConvertTo-Json -Depth 5 -Compress), $utf8)
Copy-Item -LiteralPath $zip -Destination (Join-Path $OutputDirectory $asset) -Force
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'test-ffmpeg-path.txt'), $ffmpeg, $utf8)
Print "Prepared optional FFmpeg package for StarPlayer $version, without ffprobe"
