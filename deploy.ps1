<#
Install the latest corbel release:

    irm https://raw.githubusercontent.com/erkexzcx/corbel/main/deploy.ps1 | iex

Set CORBEL_DIR to install somewhere other than %USERPROFILE%\corbel, and GITHUB_TOKEN
if the unauthenticated GitHub API rate limit gets in the way.
#>

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest is an order of magnitude slower while it draws a progress bar.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol =
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'erkexzcx/corbel'
$installDir = if ($env:CORBEL_DIR) { $env:CORBEL_DIR } else { Join-Path $HOME 'corbel' }
$userAgent = 'corbel-deploy'

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

$assetName = "corbel_${tag}_${platform}.exe"
$asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
if (-not $asset) { throw "release $tag publishes no asset named $assetName." }

$sumsName = "corbel_${tag}_SHA256SUMS.txt"
$sumsAsset = $release.assets | Where-Object { $_.name -eq $sumsName } | Select-Object -First 1

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('corbel.' + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
$binary = Join-Path $installDir 'corbel.exe'
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
    # A downloaded file keeps a mark-of-the-web stream, and the slicer launches this one without
    # anyone watching, so a SmartScreen prompt would look like the script silently doing nothing.
    Unblock-File -Path $binary -ErrorAction SilentlyContinue
} finally {
    Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
}

$version = $null
try {
    $version = & $binary --version
} catch {
    $reason = "$_"
    if ($reason -match 'Application Control|blocked this file') {
        Write-Host @"

Installed to $binary - but Windows refuses to start it:

    $reason

Smart App Control runs no program that is not signed by a certificate it already trusts, and
these builds are unsigned. The download is not the problem: this script checked it against the
release's published SHA-256 sum. Microsoft offers no per-app exception, and Unblock-File does
not lift this one. Building from source does not help either - that binary is unsigned too.

To allow it, open Windows Security -> App & browser control -> Smart App Control settings and
set it to Off. If there is no such section, the block comes from an App Control policy set by
whoever manages this PC, and only they can allow the file.
"@
        # Not `exit`: this script is meant to be run through `irm | iex`, where exiting closes
        # the window on top of the explanation.
        throw 'Windows App Control blocked corbel.exe - see above.'
    }
    $version = "corbel $tag"
}

Write-Host @"

Installed $version to $binary

corbel has two transforms and you have to name at least one, so paste one of these lines
into your slicer - PrusaSlicer: Print Settings -> Output options -> Post-processing scripts,
Orca/Bambu Studio: Others -> Post-processing Scripts:

    "$binary" --bricks --zaa      # both (start here)
    "$binary" --bricks            # BrickLayers only: interlock the walls
    "$binary" --zaa               # Z anti-aliasing only: ramp the shallow tops

Keep the quotes, and paste only the command - not the comment after it. The slicer appends the
G-code path itself. Nothing else needs setting: the layer height, the line width and the flow
are all read from the file. Bricking needs two walls or more; three or more interlocks twice as
much. Run '$binary --help' for the options.
"@
