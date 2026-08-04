<#
Install the latest bricklayers release:

    irm https://raw.githubusercontent.com/erkexzcx/Bricklayers-rust/main/deploy.ps1 | iex

Set BRICKLAYERS_DIR to install somewhere other than %USERPROFILE%\BrickLayers, and GITHUB_TOKEN
if the unauthenticated GitHub API rate limit gets in the way.
#>

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest is an order of magnitude slower while it draws a progress bar.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol =
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'erkexzcx/Bricklayers-rust'
$installDir = if ($env:BRICKLAYERS_DIR) { $env:BRICKLAYERS_DIR } else { Join-Path $HOME 'BrickLayers' }
$userAgent = 'bricklayers-deploy'

# PROCESSOR_ARCHITECTURE reports the shell's own architecture, so an x86 PowerShell on an arm64
# machine only shows the real one in PROCESSOR_ARCHITEW6432.
$architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$platform = if ($architecture -eq 'ARM64') { 'windows_arm64' } else { 'windows_amd64' }

$headers = @{ Accept = 'application/vnd.github+json' }
if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }

Write-Host "Looking up the latest release of $repo..."
try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" `
        -Headers $headers -UserAgent $userAgent
} catch {
    throw "could not reach the GitHub API: $($_.Exception.Message) If you are rate limited, set GITHUB_TOKEN and try again."
}

$tag = $release.tag_name
if (-not $tag) { throw 'the latest release has no tag name.' }

$assetName = "bricklayers_${tag}_${platform}.exe"
$asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
if (-not $asset) { throw "release $tag publishes no asset named $assetName." }

$sumsName = "bricklayers_${tag}_SHA256SUMS.txt"
$sumsAsset = $release.assets | Where-Object { $_.name -eq $sumsName } | Select-Object -First 1

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('bricklayers.' + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
$binary = Join-Path $installDir 'bricklayers.exe'
try {
    $download = Join-Path $tmp $assetName
    Write-Host "Downloading $assetName..."
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $download `
        -Headers $headers -UserAgent $userAgent

    if ($sumsAsset) {
        $sumsFile = Join-Path $tmp $sumsName
        Invoke-WebRequest -Uri $sumsAsset.browser_download_url -OutFile $sumsFile `
            -Headers $headers -UserAgent $userAgent

        $expected = Get-Content $sumsFile |
            Where-Object { ($_ -split '\s+')[1] -eq $assetName } |
            ForEach-Object { ($_ -split '\s+')[0] } |
            Select-Object -First 1
        if (-not $expected) { throw "$sumsName has no entry for $assetName." }

        $actual = (Get-FileHash -Algorithm SHA256 -Path $download).Hash
        if ($actual -ne $expected.Trim()) {
            throw 'checksum mismatch - the download is corrupt or has been tampered with.'
        }
        Write-Host 'Checksum verified.'
    } else {
        Write-Warning "release $tag publishes no checksum file, skipping verification."
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Move-Item -Force -Path $download -Destination $binary
} finally {
    Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
}

$version = try { & $binary --version } catch { "bricklayers $tag" }

Write-Host @"

Installed $version to $binary

Add this to your slicer - PrusaSlicer: Print Settings -> Output options -> Post-processing
scripts, Orca/Bambu Studio: Others -> Post-processing Scripts:

    "$binary" brick --extrusion-multiplier 1.05

Keep the quotes. The slicer appends the G-code path itself. Bricking needs two walls or more;
three or more interlocks twice as much.
"@
