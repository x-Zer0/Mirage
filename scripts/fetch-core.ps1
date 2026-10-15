<#
.SYNOPSIS
    Downloads the pinned Mihomo core and geodata into assets/core and verifies SHA256.
.DESCRIPTION
    DESIGN.md §10.1/§10.2. Files are never committed; packaging copies assets/core.
    Geodata hashes pin a specific meta-rules-dat publication. When upstream publishes
    newer data the hash check fails; update the hashes here deliberately.
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

$MihomoVersion = 'v1.19.32'
$MihomoZip = @{
    Url    = "https://github.com/MetaCubeX/mihomo/releases/download/$MihomoVersion/mihomo-windows-amd64-$MihomoVersion.zip"
    Sha256 = '1AC84E795B5B915446E20139677FB6CC013A0778E876E8D4C96F994750B4C4A3'
}
$MihomoExeSha256 = 'BEB9878924D7BD38C67176B441AB5E668CC378BFE751C8DC8FEF3EB5AAF566D5'

# meta-rules-dat "latest" release, published 2026-10-15T00:42:34Z.
$GeoBase = 'https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest'
$Geo = @(
    @{ Name = 'GEOIP.metadb'; Source = 'geoip.metadb';      Sha256 = '262BC4A119A9A3510AE957E434E11BFA41C37D72845BDA9DBEA12C3F2AC28B36' },
    @{ Name = 'GEOSITE.dat';  Source = 'geosite.dat';       Sha256 = '9AE9D588B1C051C764F2500E188393934AA84FC45E0DA96530DDF069CF232B27' },
    @{ Name = 'GEOIP.dat';    Source = 'geoip.dat';         Sha256 = '1ADAEEC24369B866B3398E8D46992F25293FDD7A7BF78135558BD767AC264D8B' },
    @{ Name = 'ASN.mmdb';     Source = 'GeoLite2-ASN.mmdb'; Sha256 = 'D82D2F39EA82A175A3D63484C8B4723040D134BA1EDF4B27D6819C2F96C14A9B' }
)

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
    Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
