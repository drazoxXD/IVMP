# Builds the IVMP client and collects everything into out\<profile>\.
#   .\build.ps1            -> out\debug\
#   .\build.ps1 -Release   -> out\release\
# The launcher is 64-bit; DLLs injected into GTAIV.exe must be 32-bit.
param([switch]$Release)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$profileName = if ($Release) { "release" } else { "debug" }
$cargoFlags = if ($Release) { @("--release") } else { @() }

# package -> target triple. Add new DLL crates here.
$builds = @(
    @{ Package = "Launcher"; Target = "x86_64-pc-windows-msvc" },
    @{ Package = "Hooks";    Target = "i686-pc-windows-msvc" }
)

$out = Join-Path "out" $profileName
New-Item -ItemType Directory -Force -Path $out | Out-Null

foreach ($b in $builds) {
    Write-Host "Building $($b.Package) for $($b.Target) ($profileName)"
    cargo build -p $b.Package --target $b.Target @cargoFlags
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

foreach ($target in ($builds | ForEach-Object { $_.Target } | Select-Object -Unique)) {
    $dir = Join-Path "target\$target" $profileName
    Get-ChildItem $dir -File | Where-Object { $_.Extension -in ".exe", ".dll", ".pdb" } |
        Copy-Item -Destination $out -Force
}

Write-Host "Output in $out"
