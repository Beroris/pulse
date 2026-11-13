# Builds a release in dist/:
#
#   pwsh scripts/package-windows.ps1
#
#   dist/Pulse-<version>-Setup.exe     the installer, which most people want
#   dist/Pulse-<version>-Portable.exe  runs from anywhere and keeps its settings beside itself
#
# The installer needs Inno Setup 6: winget install JRSoftware.InnoSetup
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    # A copy running from dist/ locks its exe, and replacing it would fail half way through.
    foreach ($f in Get-ChildItem (Join-Path $root "dist") -Filter "Pulse-*.exe" -ErrorAction SilentlyContinue) {
        try { [IO.File]::Open($f.FullName, "Open", "ReadWrite", "None").Dispose() }
        catch { throw "$($f.Name) is in use. Close Pulse first." }
    }
    cargo build --release -p tm-windows
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    $exe = Join-Path $root "target\release\pulse.exe"
    $dist = Join-Path $root "dist"
    # Clears only earlier release files: a portable copy run from here keeps its settings here too.
    New-Item -ItemType Directory -Force $dist | Out-Null
    Remove-Item (Join-Path $dist "Pulse-*") -Force -ErrorAction SilentlyContinue

    # Portable: the same exe. "Portable" in its name is what makes it keep settings beside itself.
    $portable = Join-Path $dist "Pulse-$version-Portable.exe"
    Copy-Item $exe $portable

    $iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup 6 isn't installed: winget install JRSoftware.InnoSetup" }
    & $iscc /Q "/DAppVersion=$version" "/DSourceExe=$exe" installer\pulse.iss
    if ($LASTEXITCODE -ne 0) { throw "installer build failed" }

    Write-Host "wrote $(Join-Path $dist "Pulse-$version-Setup.exe")"
    Write-Host "wrote $portable"
} finally { Pop-Location }
