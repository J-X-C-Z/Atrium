# Native x64 package and extracted-binary acceptance. PowerShell 7.
param([Parameter(Mandatory)][string]$Output)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
if (!$IsWindows -or [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Build this package on x64 Windows.'
}
$archive = [IO.Path]::GetFullPath($Output)
$root = [IO.Directory]::CreateTempSubdirectory('atrium-node-package-').FullName
$target = Join-Path $root 'target'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    $hostTriple = ((rustc +stable -vV | Select-String '^host: ').Line -replace '^host: ')
    if ($hostTriple -ne 'x86_64-pc-windows-msvc') { throw "Use native MSVC Rust: $hostTriple" }
    if (Test-Path -LiteralPath $archive) { throw "Output already exists: $archive" }
    cargo +stable build --locked --release -p atrium-node --bin atrium-node --target-dir $target
    $binary = Join-Path $target 'release/atrium-node.exe'
    $stage = New-Item -ItemType Directory -Path (Join-Path $root 'stage')
    Copy-Item -LiteralPath $binary -Destination $stage.FullName
    $staged = Join-Path $stage.FullName 'atrium-node.exe'
    $manifest = [ordered]@{
        package = 'atrium-node'
        target = $hostTriple
        source_commit = (git rev-parse HEAD)
        source_dirty = [bool](git status --porcelain)
        binary = 'atrium-node.exe'
        sha256 = (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $manifest | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stage.FullName 'manifest.json') -Encoding utf8
    $tempArchive = Join-Path $root 'package.zip'
    Compress-Archive -LiteralPath @($staged, (Join-Path $stage.FullName 'manifest.json')) -DestinationPath $tempArchive
    $verify = Join-Path $root 'verify'
    Expand-Archive -LiteralPath $tempArchive -DestinationPath $verify
    $extracted = Join-Path $verify 'atrium-node.exe'
    $saved = Get-Content -LiteralPath (Join-Path $verify 'manifest.json') -Raw | ConvertFrom-Json
    if ((Get-ChildItem -LiteralPath $verify -Force).Count -ne 2 -or
        $saved.sha256 -ne $manifest.sha256 -or
        (Get-FileHash -LiteralPath $extracted -Algorithm SHA256).Hash -ne $saved.sha256) {
        throw 'Package contents failed verification.'
    }
    $oldBinary = $env:ATRIUM_TEST_NODE_BINARY
    try {
        $env:ATRIUM_TEST_NODE_BINARY = $extracted
        cargo +stable test --locked --release -p atrium-node --test protocol --target-dir $target
    } finally {
        $env:ATRIUM_TEST_NODE_BINARY = $oldBinary
    }
    [IO.File]::Copy($tempArchive, $archive, $false)
    Get-FileHash -LiteralPath $archive -Algorithm SHA256
} finally {
    Pop-Location
    Remove-Item -LiteralPath $root -Force -Recurse
}
