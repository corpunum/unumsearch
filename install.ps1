# unumsearch installer for Windows (x86_64).
#
#   irm https://github.com/corpunum/unumsearch/releases/latest/download/install.ps1 | iex
#
# Downloads the prebuilt binary from GitHub Releases, verifies it against the
# release's SHA256SUMS and installs it into $env:UNUMSEARCH_INSTALL_DIR
# (default %LOCALAPPDATA%\Programs\unumsearch), adding that folder to the user PATH.
#
# Environment:
#   UNUMSEARCH_VERSION      tag to install (default: latest release), e.g. v0.1.0
#   UNUMSEARCH_INSTALL_DIR  destination directory
#   UNUMSEARCH_REPO         owner/repo (default: corpunum/unumsearch)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = if ($env:UNUMSEARCH_REPO) { $env:UNUMSEARCH_REPO } else { 'corpunum/unumsearch' }
$version = if ($env:UNUMSEARCH_VERSION) { $env:UNUMSEARCH_VERSION } else { 'latest' }
$dest = if ($env:UNUMSEARCH_INSTALL_DIR) { $env:UNUMSEARCH_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\unumsearch' }

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64' -and $env:PROCESSOR_ARCHITEW6432 -ne 'AMD64') {
  # ARM64 Windows runs the x86_64 build under emulation.
  if ($arch -ne 'ARM64') { throw "unsupported architecture: $arch" }
}
$target = 'x86_64-pc-windows-msvc'

if ($version -eq 'latest') {
  $rel = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$repo/releases/latest"
  $version = $rel.tag_name
}
$base = "https://github.com/$repo/releases/download/$version"
$name = "unumsearch-$version-$target"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("unumsearch-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "unumsearch-install: downloading $name.zip"
  Invoke-WebRequest -UseBasicParsing "$base/$name.zip" -OutFile (Join-Path $tmp "$name.zip")
  Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')
  $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { ($_ -split '\s+')[1] -replace '^\*', '' -eq "$name.zip" } | Select-Object -First 1
  if (-not $line) { throw "$name.zip is not listed in SHA256SUMS" }
  $expected = ($line -split '\s+')[0].ToLower()
  $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp "$name.zip")).Hash.ToLower()
  if ($expected -ne $actual) { throw "checksum mismatch for $name.zip (expected $expected, got $actual)" }
  Write-Host 'unumsearch-install: checksum ok'
  Expand-Archive -Path (Join-Path $tmp "$name.zip") -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  $exe = Join-Path $dest 'unumsearch.exe'
  # A running daemon locks the exe; rename it aside first (Windows allows renaming a running image).
  if (Test-Path $exe) { Move-Item -Force $exe "$exe.old" -ErrorAction SilentlyContinue }
  Copy-Item -Force (Join-Path $tmp "$name\unumsearch.exe") $exe
  Remove-Item -Force "$exe.old" -ErrorAction SilentlyContinue
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not (($userPath -split ';') -contains $dest)) {
    [Environment]::SetEnvironmentVariable('Path', ($(if ($userPath) { "$userPath;" } else { '' }) + $dest), 'User')
    Write-Host "unumsearch-install: added $dest to the user PATH (open a new shell)"
  }
  Write-Host "unumsearch-install: installed $(& $exe --version) to $exe"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
