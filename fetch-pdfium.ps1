# Downloads the latest bblanchon/pdfium-binaries pdfium.dll (win-x64) into
# target\debug and target\release so `cargo run` / `cargo test` find it.
$ErrorActionPreference = 'Stop'

$release = Invoke-RestMethod `
    -Uri 'https://api.github.com/repos/bblanchon/pdfium-binaries/releases/latest' `
    -Headers @{ Accept = 'application/vnd.github+json'; 'User-Agent' = 'pppdf-fetch' }
$asset = $release.assets | Where-Object name -EQ 'pdfium-win-x64.tgz'
if (-not $asset) { throw "pdfium-win-x64.tgz not found in release $($release.tag_name)" }

$work = Join-Path ([IO.Path]::GetTempPath()) ("pdfium-" + ($release.tag_name -replace '/', '-'))
New-Item -ItemType Directory -Force $work | Out-Null
$tgz = Join-Path $work 'pdfium-win-x64.tgz'
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tgz
tar -xzf $tgz -C $work
if ($LASTEXITCODE -ne 0) { throw 'tar extraction failed' }

foreach ($dir in @("$PSScriptRoot\target\debug", "$PSScriptRoot\target\release")) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item (Join-Path $work 'bin\pdfium.dll') $dir -Force
}
Write-Host "pdfium.dll $($release.tag_name) -> target\debug, target\release"
