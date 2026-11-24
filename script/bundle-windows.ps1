#Requires -Version 5.1
# Builds the portable ZIP and, with -Installer, the per-user installer EXE on
# x64 Windows with MSVC and the Windows SDK. GPUI 0.3.8 uses fxc.exe for
# release shaders. Set GPUI_FXC_PATH if SDK discovery fails.
param(
    [switch]$Installer,
    [switch]$Check
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT' -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Build the Atrium desktop package on x64 Windows.'
}

$target = 'x86_64-pc-windows-msvc'
Push-Location (Join-Path $PSScriptRoot '..')
$stage = $null
$originalFxc = $env:GPUI_FXC_PATH
try {
    $rustHost = ((rustc -vV | Select-String '^host: ').Line -replace '^host: ')
    if ($rustHost -ne $target) { throw "Use a native x64 MSVC Rust toolchain. Found $rustHost." }

    if (!$env:GPUI_FXC_PATH) {
        $fxc = Get-Command fxc.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($fxc) {
            $env:GPUI_FXC_PATH = $fxc.Source
        } else {
            $kits = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots').KitsRoot10
            $fxc = Get-ChildItem (Join-Path $kits 'bin\10.*\x64\fxc.exe') |
                Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
            $env:GPUI_FXC_PATH = $fxc.FullName
        }
    }
    if (!$env:GPUI_FXC_PATH -or !(Test-Path -LiteralPath $env:GPUI_FXC_PATH -PathType Leaf)) {
        throw 'Install the Windows SDK with x64 fxc.exe or set GPUI_FXC_PATH to its full path.'
    }

    $metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
    $version = ($metadata.packages | Where-Object { $_.name -eq 'atrium-app' } | Select-Object -First 1).version
    if (!$version) { throw 'atrium-app version missing from cargo metadata.' }

    if ($Check) {
        $iscc = Get-Command ISCC.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        Write-Output "target: $target"
        Write-Output "version: $version"
        Write-Output "fxc: $env:GPUI_FXC_PATH"
        Write-Output "inno setup: $(if ($iscc) { $iscc.Source } else { 'not found' })"
        return
    }

    cargo build --locked --release --target $target -p atrium-app --bin atrium -p atrium-node --bin atrium-node
    if ($LASTEXITCODE -ne 0) { throw 'Atrium release build failed.' }
    $binary = Join-Path $metadata.target_directory "$target/release/atrium.exe"
    $nodeBinary = Join-Path $metadata.target_directory "$target/release/atrium-node.exe"

    $dist = New-Item -ItemType Directory -Path 'dist' -Force
    $stage = Join-Path $dist.FullName ".atrium-bundle.$([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $stage | Out-Null
    $zip = Join-Path $stage 'Atrium-windows-x64.zip'
    Compress-Archive -LiteralPath @($binary, $nodeBinary) -DestinationPath $zip
    $verify = Join-Path $stage 'verify'
    Expand-Archive -LiteralPath $zip -DestinationPath $verify
    $extracted = Join-Path $verify 'atrium.exe'
    $extractedNode = Join-Path $verify 'atrium-node.exe'

    python script/smoke-desktop.py $extracted
    if ($LASTEXITCODE -ne 0) { throw 'The packaged executable failed its MCP stdio smoke.' }
    & $extractedNode config path | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'The packaged atrium-node failed to start.' }

    $output = Join-Path $dist.FullName 'Atrium-windows-x64.zip'
    Move-Item -LiteralPath $zip -Destination $output -Force
    Write-Output "Built $output"

    if ($Installer) {
        $iscc = (Get-Command ISCC.exe -CommandType Application -ErrorAction SilentlyContinue |
            Select-Object -First 1).Source
        if (!$iscc) {
            foreach ($candidate in @(
                (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
                (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
            )) {
                if (Test-Path -LiteralPath $candidate -PathType Leaf) { $iscc = $candidate; break }
            }
        }
        if (!$iscc) {
            throw 'Inno Setup 6 is required for -Installer. Install it with: choco install innosetup --yes'
        }
        & $iscc "/DAppVersion=$version" "/DAppExe=$binary" "/DNodeExe=$nodeBinary" `
            "/O$($dist.FullName)" 'packaging/windows/atrium.iss'
        if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed.' }
        $installer = Join-Path $dist.FullName "Atrium-$version-windows-x64-setup.exe"
        if (!(Test-Path -LiteralPath $installer -PathType Leaf)) {
            throw "The installer was not written to $installer"
        }
        Write-Output "Built $installer"
    }
} finally {
    $env:GPUI_FXC_PATH = $originalFxc
    if ($stage -and (Test-Path -LiteralPath $stage)) { Remove-Item -LiteralPath $stage -Recurse -Force }
    Pop-Location
}
