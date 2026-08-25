# One-line installer for the nirdosha CLI (Windows).
#
#   irm https://raw.githubusercontent.com/arunsoman/nirdosha/main/scripts/install.ps1 | iex
#
# Downloads the prebuilt x86_64 binary from GitHub Releases, verifies its
# sha256 checksum, and installs it to $env:NIRDOSHA_INSTALL_DIR (default
# %LOCALAPPDATA%\nirdosha\bin). No Rust or z3 required -- the binary has
# Z3 statically vendored. A native C toolchain (e.g. clang, installable
# via `winget install LLVM.LLVM`) is still needed if you later run
# `nirdosha build` (native codegen); interpreting/`emit-ui`/`serve` work
# with no extra install.

$ErrorActionPreference = "Stop"

$repo = "arunsoman/nirdosha"
$installDir = if ($env:NIRDOSHA_INSTALL_DIR) { $env:NIRDOSHA_INSTALL_DIR } else { "$env:LOCALAPPDATA\nirdosha\bin" }
$version = if ($env:NIRDOSHA_VERSION) { $env:NIRDOSHA_VERSION } else { "latest" }
$asset = "nirdosha-x86_64-pc-windows-msvc.zip"

if ($version -eq "latest") {
    $url = "https://github.com/$repo/releases/latest/download/$asset"
    $checksumUrl = "https://github.com/$repo/releases/latest/download/$asset.sha256"
} else {
    $url = "https://github.com/$repo/releases/download/$version/$asset"
    $checksumUrl = "https://github.com/$repo/releases/download/$version/$asset.sha256"
}

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "nirdosha-install-$([guid]::NewGuid())")
try {
    $zipPath = Join-Path $tmp $asset
    Write-Host "Downloading $asset ($version)..."
    Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing

    $checksumPath = Join-Path $tmp "$asset.sha256"
    try {
        Invoke-WebRequest -Uri $checksumUrl -OutFile $checksumPath -UseBasicParsing
        $expected = (Get-Content $checksumPath -Raw).Trim().Split(" ")[0]
        $actual = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
        if ($expected.ToLower() -ne $actual) {
            throw "checksum mismatch for $asset (expected $expected, got $actual)"
        }
        Write-Host "Checksum verified."
    } catch {
        Write-Host "Warning: could not verify checksum ($_)"
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
    Copy-Item -Path (Join-Path $tmp "nirdosha.exe") -Destination (Join-Path $installDir "nirdosha.exe") -Force

    Write-Host "Installed nirdosha to $installDir\nirdosha.exe"
    if (($env:Path -split ";") -notcontains $installDir) {
        Write-Host "Add it to your PATH: `$env:Path += \";$installDir\"`  (or add it permanently via System Properties)"
    }
    Write-Host "Try it: nirdosha            # prints usage"
    Write-Host "        nirdosha hello.nir  # see README.md for a hello-world snippet to paste"
} finally {
    Remove-Item -Recurse -Force $tmp
}
