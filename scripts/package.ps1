<#
.SYNOPSIS
    Build the Windows x64 installer and portable ZIP; requires Inno Setup and cargo-about.
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$Iscc,
    [string]$CargoAbout,
    [switch]$SkipBuild,
    [switch]$StageOnly
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Pinned = Import-PowerShellDataFile (Join-Path $PSScriptRoot 'core-assets.psd1')

function Resolve-Tool([string]$Explicit, [string]$Name, [string[]]$Candidates) {
    if ($Explicit) { return (Resolve-Path -LiteralPath $Explicit).Path }
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw "Missing $Name. Supply its path explicitly; see docs/packaging.md."
}

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed ($LASTEXITCODE)" }
}

function Copy-Core([string]$Stage) {
    $shared = Get-Content -LiteralPath (Join-Path $Root 'ipc/src/binary.rs') -Raw
    if (-not $shared.Contains($Pinned.MihomoExeSha256.ToLowerInvariant())) {
        throw 'Core pin differs from the application/service pin.'
    }
    $files = @(@{ Name = 'mihomo.exe'; Sha256 = $Pinned.MihomoExeSha256 }) + $Pinned.Geo
    foreach ($file in $files) {
        $source = Join-Path $Root "assets/core/$($file.Name)"
        if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne $file.Sha256) {
            throw "Invalid bundled asset: $($file.Name). Run scripts/fetch-core.ps1."
        }
        Copy-Item -LiteralPath $source -Destination (Join-Path $Stage 'core') -Force
    }
}

function Write-SlintNotices([string]$Stage) {
    $metadata = cargo metadata --locked --offline --filter-platform x86_64-pc-windows-msvc --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'License metadata failed' }
    $crates = $metadata.packages | Where-Object { $_.license -match 'LicenseRef-Slint-Royalty-free' }
    $lines = @('Slint 1.18.1 — Royalty-free License 2.0', 'Full text: Slint-Royalty-free-2.0.md', '')
    foreach ($crate in $crates | Sort-Object name) {
        $lines += "$($crate.name) $($crate.version) — $($crate.repository)"
    }
    $lines | Set-Content -LiteralPath "$Stage/licenses/Slint-components.txt" -Encoding utf8
}

function Write-PayloadHashes([string]$Stage) {
    $lines = Get-ChildItem -LiteralPath $Stage -File -Recurse | Sort-Object FullName | ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($Stage, $_.FullName).Replace('\', '/')
        if ($relative -notin @('payload-sha256.txt', 'app.ico')) {
            '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative
        }
    }
    $lines | Set-Content -LiteralPath (Join-Path $Stage 'payload-sha256.txt') -Encoding utf8
}

function Initialize-Payload([string]$Stage, [string]$Release, [string]$CargoAbout) {
    New-Item -ItemType Directory -Force -Path $stage, "$stage/core", "$stage/licenses" | Out-Null
    foreach ($name in @('Mirage.exe', 'mirage-service.exe')) {
        Copy-Item -LiteralPath (Join-Path $release $name) -Destination $stage -Force
    }
    $icon = Get-ChildItem -Path "$release/build/mirage-*/out/app.ico" | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
    if (-not $icon) { throw 'Build-generated application icon missing.' }
    Copy-Item -LiteralPath $icon.FullName -Destination "$stage/app.ico" -Force
    Copy-Core $stage
    Copy-Item -Path 'packaging/licenses/*' -Destination "$stage/licenses" -Recurse -Force
    Copy-Item -LiteralPath 'packaging/THIRD-PARTY-NOTICES.txt' -Destination $stage -Force
    Invoke-Checked $CargoAbout @('generate', '--workspace', '--locked', '--offline', '--fail', '--target', 'x86_64-pc-windows-msvc', '--config', 'packaging/about.toml', '--output-file', "$stage/licenses/rust-dependencies.html", 'packaging/licenses.hbs')
    $licenseList = Get-Content -LiteralPath "$stage/licenses/rust-dependencies.html" -Raw
    foreach ($crate in @('reqwest', 'rustls', 'tokio', 'serde', 'windows')) {
        if (-not $licenseList.Contains($crate)) { throw "Incomplete license report: $crate" }
    }
    Write-SlintNotices $stage
    Write-PayloadHashes $stage
    $required = @('Mihomo-GPL-3.0.txt', 'Slint-Royalty-free-2.0.md', 'CC-BY-SA-4.0.txt',
        'Lucide-LICENSE.txt', 'Inno-Setup.txt', 'geodata-notices.txt')
    foreach ($name in $required) {
        if ((Get-Item -LiteralPath "$stage/licenses/$name").Length -eq 0) {
            throw "Empty license text: $name"
        }
    }
}

function Write-ArtifactHash([string]$Path) {
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $(Split-Path $Path -Leaf)" | Set-Content -LiteralPath "$Path.sha256" -Encoding ascii
}

function New-PortableArchive([string]$Stage, [string]$Output, [string]$Version) {
    $portable = Join-Path $Stage 'portable/Mirage'
    New-Item -ItemType Directory -Force -Path $portable | Out-Null
    foreach ($name in @('Mirage.exe', 'mirage-service.exe', 'core', 'licenses', 'THIRD-PARTY-NOTICES.txt')) {
        Copy-Item -LiteralPath (Join-Path $Stage $name) -Destination $portable -Recurse -Force
    }
    Copy-Item -LiteralPath (Join-Path $Root 'packaging/README-portable.txt') -Destination $portable -Force
    Write-PayloadHashes $portable
    $temporaryZip = Join-Path $Stage 'portable.zip'
    Compress-Archive -LiteralPath $portable -DestinationPath $temporaryZip -CompressionLevel Optimal
    $archive = Join-Path $Output "Mirage-$Version-windows-x64-portable.zip"
    Move-Item -LiteralPath $temporaryZip -Destination $archive -Force
    Write-ArtifactHash $archive
    Write-Host "Portable: $archive"
}

Push-Location $Root
try {
    $metadata = cargo metadata --locked --offline --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $app = $metadata.packages | Where-Object name -EQ mirage
    $helper = $metadata.packages | Where-Object name -EQ mirage-service
    if ($app.version -ne $helper.version -or $app.version -notmatch '^\d+\.\d+\.\d+$') {
        throw 'App and helper must have the same numeric release version.'
    }
    $hostInfo = rustc -vV
    if ($LASTEXITCODE -ne 0 -or -not ($hostInfo -contains 'host: x86_64-pc-windows-msvc')) {
        throw 'Run packaging with the x86_64-pc-windows-msvc Rust toolchain.'
    }
    $version = $app.version
    $aboutCandidates = @(Get-ChildItem -Path (Join-Path $Root 'target/tools/cargo-about') -Filter cargo-about.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName)
    $CargoAbout = Resolve-Tool $CargoAbout 'cargo-about.exe' $aboutCandidates
    if (-not $StageOnly) {
        $Iscc = Resolve-Tool $Iscc 'ISCC.exe' @(
            (Join-Path $Root 'target/tools/innosetup-6.4.3/ISCC.exe'),
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
        )
    }
    & (Join-Path $PSScriptRoot 'fetch-core.ps1')
    if (-not $SkipBuild) {
        Invoke-Checked 'cargo' @('build', '--workspace', '--release', '--locked', '-j', '1')
    }
    $release = Join-Path $metadata.target_directory 'release'
    $stage = Join-Path $Root ("target/package/$version-" + [Guid]::NewGuid().ToString('N'))
    Initialize-Payload $stage $release $CargoAbout
    if ($StageOnly) { Write-Host "Staged: $stage"; return }
    $output = Join-Path $Root 'dist'
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    Invoke-Checked $Iscc @("/DAppVersion=$version", "/DStageDir=$stage", "/DOutputDir=$output", 'packaging/Mirage.iss')
    $setup = Join-Path $output "Mirage-$version-windows-x64-setup.exe"
    Write-ArtifactHash $setup
    Write-Host "Installer: $setup"
    New-PortableArchive $stage $output $version
} finally {
    Pop-Location
}
