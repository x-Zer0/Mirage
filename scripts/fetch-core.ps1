<#
.SYNOPSIS
    Downloads the pinned Mihomo core and geodata into assets/core and verifies SHA256.
.DESCRIPTION
    DESIGN.md §10.1/§10.2. Files are never committed; packaging copies assets/core.
    Geodata URLs pin a meta-rules-dat commit and verify its file hashes.
    Update the snapshot commit and hashes together in core-assets.psd1.
#>
[CmdletBinding()]
param(
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Root = Split-Path -Parent $PSScriptRoot
$Dest = Join-Path $Root 'assets/core'
$Tmp  = Join-Path ([IO.Path]::GetTempPath()) ("mirage-core-" + [Guid]::NewGuid())

$Pinned = Import-PowerShellDataFile (Join-Path $PSScriptRoot 'core-assets.psd1')
$MihomoVersion = $Pinned.MihomoVersion
$MihomoZip = $Pinned.MihomoZip
$MihomoExeSha256 = $Pinned.MihomoExeSha256
$GeoBase = $Pinned.GeoBase
$Geo = $Pinned.Geo

function Get-Verified([string]$Url, [string]$Expected, [string]$OutFile) {
    Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $OutFile).Hash
    if ($actual -ne $Expected) {
        Remove-Item -LiteralPath $OutFile -Force
        throw "SHA256 mismatch for $Url`n expected $Expected`n actual   $actual"
    }
}

function Test-Present([string]$Path, [string]$Expected) {
    (Test-Path -LiteralPath $Path) -and ((Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash -eq $Expected)
}

New-Item -ItemType Directory -Force -Path $Dest, $Tmp | Out-Null
try {
    $exe = Join-Path $Dest 'mihomo.exe'
    if ($Force -or -not (Test-Present $exe $MihomoExeSha256)) {
        Write-Host "Fetching Mihomo $MihomoVersion"
        $zip = Join-Path $Tmp 'mihomo.zip'
        Get-Verified $MihomoZip.Url $MihomoZip.Sha256 $zip
        Expand-Archive -LiteralPath $zip -DestinationPath $Tmp -Force
        $inner = Get-ChildItem -LiteralPath $Tmp -Filter 'mihomo-windows-amd64*.exe' | Select-Object -First 1
        if (-not $inner) { throw 'mihomo exe not found in archive' }
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $inner.FullName).Hash -ne $MihomoExeSha256) {
            throw 'extracted mihomo.exe hash mismatch'
        }
        Move-Item -LiteralPath $inner.FullName -Destination $exe -Force
    } else {
        Write-Host 'mihomo.exe up to date'
    }

    foreach ($g in $Geo) {
        $target = Join-Path $Dest $g.Name
        if ($Force -or -not (Test-Present $target $g.Sha256)) {
            Write-Host "Fetching $($g.Name)"
            $file = Join-Path $Tmp $g.Name
            Get-Verified "$GeoBase/$($g.Source)" $g.Sha256 $file
            Move-Item -LiteralPath $file -Destination $target -Force
        } else {
            Write-Host "$($g.Name) up to date"
        }
    }

    $version = & $exe -v
    Write-Host "Core ready: $version"
} finally {
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ([IO.Path]::GetFullPath($Tmp).StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path $Tmp -Leaf).StartsWith('mirage-core-')) {
        Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}
