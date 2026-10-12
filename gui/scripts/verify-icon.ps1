<#
.SYNOPSIS
G-4 v2: verifies that the tracked icon assets reproduce byte-identically from
their SVG sources through the full render -> tauri icon -> compose pipeline.

.DESCRIPTION
Reports six independent measurements against -AppIconPath / -IcoPath (both
read-only, defaulting to the tracked assets):
  (1) master determinism: re-rendering icon.svg matches -AppIconPath;
  (2) full pipeline integrity: -AppIconPath through `tauri icon` (CWD = gui/)
      and compose-ico.ps1 reproduces -IcoPath (2a) and the tracked
      32x32.png/128x128.png/128x128@2x.png/icon.png (2b);
  (3) ICO structure: -IcoPath has exactly the 16/24/32/48/64/256 entries
      (ICONDIRENTRY 0 means 256), each PNG-encoded;
  (4) transparency: -AppIconPath has alpha 0 at all four corners and at
      least one fully opaque pixel;
  (5) simplified glyph direct measurement: icon-small.svg rendered and
      downscaled independently of compose-ico.ps1 matches -IcoPath's
      16/24/32 entries byte-for-byte;
  (6) master pixel spot-check: -AppIconPath matches the frozen expected
      alpha (and colour, where opaque) at the sample points that pin the
      artwork's geometry -- byte determinism alone cannot flag a
      mis-render once it has been committed as the master.
Every measurement is reported PASS or FAIL; the process exits 1 if any
measurement failed. Nothing under source control is written to. Internal
child-process invocations (of render-icon.ps1 and compose-ico.ps1) launch via
the absolute path of Windows PowerShell 5.1, not the pwsh resolved from PATH.

.PARAMETER ScratchDir
Working directory for intermediate renders and pipeline runs. Must resolve
outside the repository, and must not resolve through a reparse point
(junction or symlink) at any existing path component -- this is checked
before the repository-membership test, so a reparse point that only appears
to point outside the repository is rejected too.

.PARAMETER AppIconPath
The 1024x1024 master PNG to verify. Defaults to the tracked app-icon.png.

.PARAMETER IcoPath
The composed icon.ico to verify. Defaults to the tracked icons/icon.ico.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$ScratchDir,
  [string]$AppIconPath,
  [string]$IcoPath
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
. (Join-Path $PSScriptRoot 'ico-common.ps1')

$systemPowerShellPath = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'

function Test-PathHasReparsePoint {
  <#
  Walks NormalizedPath up through its existing ancestors (excluding the drive
  root) and returns true if any of them carries the ReparsePoint attribute
  (a junction or symlink). GetFullPath alone does not resolve these, so a
  path that is lexically outside a boundary can still physically point
  inside it.
  #>
  param([Parameter(Mandatory = $true)][string]$NormalizedPath)
  $current = $NormalizedPath.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
  while ($current) {
    $isDriveRoot = $current -match '^[A-Za-z]:\\?$'
    if (-not $isDriveRoot -and (Test-Path -LiteralPath $current)) {
      $attributes = [System.IO.File]::GetAttributes($current)
      if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        return $true
      }
    }
    if ($isDriveRoot) { break }
    $parent = Split-Path -Parent $current
    if ([string]::IsNullOrEmpty($parent) -or $parent -eq $current) { break }
    $current = $parent
  }
  return $false
}

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
if (-not $repoRoot.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
  $repoRoot += [System.IO.Path]::DirectorySeparatorChar
}
$scratchFull = [System.IO.Path]::GetFullPath($ScratchDir)
if (Test-PathHasReparsePoint -NormalizedPath $scratchFull) {
  throw "verify-icon: -ScratchDir must not resolve through a reparse point (junction or symlink), found $scratchFull"
}
$scratchFullChecked = $scratchFull
if (-not $scratchFullChecked.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
  $scratchFullChecked += [System.IO.Path]::DirectorySeparatorChar
}
if ($scratchFullChecked.StartsWith($repoRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "verify-icon: -ScratchDir must resolve outside the repository, found $scratchFull"
}
New-Item -ItemType Directory -Path $scratchFull -Force | Out-Null

$appIconPathDefault = Join-Path $PSScriptRoot '..\src-tauri\app-icon.png'
$icoPathDefault = Join-Path $PSScriptRoot '..\src-tauri\icons\icon.ico'
$appIconPathRaw = if ($PSBoundParameters.ContainsKey('AppIconPath')) { $AppIconPath } else { $appIconPathDefault }
$icoPathRaw = if ($PSBoundParameters.ContainsKey('IcoPath')) { $IcoPath } else { $icoPathDefault }
foreach ($required in @($appIconPathRaw, $icoPathRaw)) {
  if (-not (Test-Path -LiteralPath $required)) { throw "verify-icon: not found: $required" }
}
$resolvedAppIconPath = (Resolve-Path -LiteralPath $appIconPathRaw).Path
$resolvedIcoPath = (Resolve-Path -LiteralPath $icoPathRaw).Path

$guiDir = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$renderScript = Join-Path $PSScriptRoot 'render-icon.ps1'
$composeScript = Join-Path $PSScriptRoot 'compose-ico.ps1'
$smallSvgPath = Join-Path $PSScriptRoot '..\src-tauri\icons\icon-small.svg'
$trackedIconsDir = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\src-tauri\icons'))

$failures = 0
function Write-Measurement {
  param([string]$Number, [string]$Label, [bool]$Passed, [string]$Detail)
  if ($Passed) {
    Write-Output "($Number) PASS ${Label}: $Detail"
  }
  else {
    Write-Output "($Number) FAIL ${Label}: $Detail"
    $script:failures++
  }
}

function Test-BytesEqual {
  param([byte[]]$Left, [byte[]]$Right)
  return [System.Linq.Enumerable]::SequenceEqual($Left, $Right)
}

function Get-BitmapArgbBytes {
  param([Parameter(Mandatory = $true)][System.Drawing.Bitmap]$Bitmap)
  $rect = New-Object System.Drawing.Rectangle(0, 0, $Bitmap.Width, $Bitmap.Height)
  $data = $Bitmap.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  try {
    $byteCount = [Math]::Abs($data.Stride) * $data.Height
    $bytes = New-Object byte[] $byteCount
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $byteCount)
    return [pscustomobject]@{ Bytes = $bytes; Stride = $data.Stride }
  }
  finally { $Bitmap.UnlockBits($data) }
}

function Test-SpotPixel {
  <#
  Compares one pixel of Bitmap against a frozen expectation and returns both
  the verdict and, when it fails, the fixed-format Detail fragment. Reading
  through GetPixel keeps the expectations in A/R/G/B order, so they cannot be
  transposed against the BGRA byte order a raw scan would yield. Transparent
  sample points compare alpha only, because straight-alpha RGB under a zero
  alpha is not guaranteed by the encoder.
  #>
  param(
    [Parameter(Mandatory = $true)][System.Drawing.Bitmap]$Bitmap,
    [Parameter(Mandatory = $true)][int]$X,
    [Parameter(Mandatory = $true)][int]$Y,
    [Parameter(Mandatory = $true)][int]$ExpectedA,
    [int]$ExpectedR = 0,
    [int]$ExpectedG = 0,
    [int]$ExpectedB = 0,
    [Parameter(Mandatory = $true)][bool]$Opaque
  )
  $pixel = $Bitmap.GetPixel($X, $Y)
  if ($Opaque) {
    $ok = ($pixel.A -eq $ExpectedA) -and ($pixel.R -eq $ExpectedR) -and ($pixel.G -eq $ExpectedG) -and ($pixel.B -eq $ExpectedB)
    $expectedText = "A=$ExpectedA,R=$ExpectedR,G=$ExpectedG,B=$ExpectedB"
  }
  else {
    $ok = ($pixel.A -eq $ExpectedA)
    $expectedText = "A=$ExpectedA"
  }
  $detail = ''
  if (-not $ok) {
    $actualText = "A=$($pixel.A),R=$($pixel.R),G=$($pixel.G),B=$($pixel.B)"
    $detail = "mismatch at ($X,$Y): expected $expectedText actual $actualText"
  }
  return [pscustomobject]@{ Passed = $ok; Detail = $detail }
}

# (1) Master determinism.
try {
  $m1Out = Join-Path $scratchFull 'm1-render.png'
  & $systemPowerShellPath -ExecutionPolicy Bypass -File $renderScript -OutPath $m1Out | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "render-icon.ps1 exited with code $LASTEXITCODE" }
  $renderedBytes = [System.IO.File]::ReadAllBytes($m1Out)
  $appIconBytes = [System.IO.File]::ReadAllBytes($resolvedAppIconPath)
  $pass1 = Test-BytesEqual -Left $renderedBytes -Right $appIconBytes
  Write-Measurement -Number '1' -Label 'master determinism' -Passed $pass1 -Detail "$($renderedBytes.Length) bytes vs $($appIconBytes.Length) bytes"
}
catch {
  Write-Measurement -Number '1' -Label 'master determinism' -Passed $false -Detail $_.Exception.Message
}

# (2) Full pipeline integrity: -AppIconPath -> tauri icon -> compose-ico.ps1.
try {
  $m2TauriOut = Join-Path $scratchFull 'm2-tauri'
  New-Item -ItemType Directory -Path $m2TauriOut -Force | Out-Null
  $previousLocation = Get-Location
  try {
    Set-Location -LiteralPath $guiDir
    & npx @tauri-apps/cli icon $resolvedAppIconPath -o $m2TauriOut | Out-Null
    $tauriExit = $LASTEXITCODE
  }
  finally { Set-Location -LiteralPath $previousLocation }
  if ($tauriExit -ne 0) { throw "tauri icon exited with code $tauriExit" }

  $m2SourceIco = Join-Path $m2TauriOut 'icon.ico'
  $m2Ico2 = Join-Path $scratchFull 'm2-icon2.ico'
  & $systemPowerShellPath -ExecutionPolicy Bypass -File $composeScript -SourceIco $m2SourceIco -OutPath $m2Ico2 | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "compose-ico.ps1 exited with code $LASTEXITCODE" }

  $composedBytes = [System.IO.File]::ReadAllBytes($m2Ico2)
  $trackedIcoBytes = [System.IO.File]::ReadAllBytes($resolvedIcoPath)
  $pass2a = Test-BytesEqual -Left $composedBytes -Right $trackedIcoBytes
  Write-Measurement -Number '2a' -Label 'full pipeline ICO' -Passed $pass2a -Detail "$($composedBytes.Length) bytes vs $($trackedIcoBytes.Length) bytes"

  $comparisonFiles = @('32x32.png', '128x128.png', '128x128@2x.png', 'icon.png')
  $mismatched = @()
  foreach ($name in $comparisonFiles) {
    $scratchBytes = [System.IO.File]::ReadAllBytes((Join-Path $m2TauriOut $name))
    $trackedBytes = [System.IO.File]::ReadAllBytes((Join-Path $trackedIconsDir $name))
    if (-not (Test-BytesEqual -Left $scratchBytes -Right $trackedBytes)) { $mismatched += $name }
  }
  $pass2b = ($mismatched.Count -eq 0)
  $detail2b = if ($pass2b) { "$($comparisonFiles.Count) files identical" } else { "mismatched: $($mismatched -join ', ')" }
  Write-Measurement -Number '2b' -Label 'full pipeline PNG set' -Passed $pass2b -Detail $detail2b
}
catch {
  Write-Measurement -Number '2a' -Label 'full pipeline ICO' -Passed $false -Detail $_.Exception.Message
  Write-Measurement -Number '2b' -Label 'full pipeline PNG set' -Passed $false -Detail $_.Exception.Message
}

# (3) ICO structure: 16/24/32/48/64/256, all PNG-encoded.
try {
  $icoBytes = [System.IO.File]::ReadAllBytes($resolvedIcoPath)
  $parsed = ConvertFrom-IcoBytes -Bytes $icoBytes
  $expectedSizes = @(16, 24, 32, 48, 64, 256) | Sort-Object
  $actualSizes = @($parsed.Entries | ForEach-Object { $_.Width }) | Sort-Object
  $allSquare = -not ($parsed.Entries | Where-Object { $_.Width -ne $_.Height })
  $sizesMatch = ($parsed.Entries.Count -eq 6) -and (($actualSizes -join ',') -eq ($expectedSizes -join ','))
  $pngSignature = [byte[]]@(0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A)
  $allPng = $true
  foreach ($entry in $parsed.Entries) {
    if ($entry.Data.Length -lt 8 -or -not (Test-BytesEqual -Left $entry.Data[0..7] -Right $pngSignature)) {
      $allPng = $false
      break
    }
  }
  $pass3 = $sizesMatch -and $allSquare -and $allPng
  $detail3 = "entries=$($parsed.Entries.Count) sizes=$($actualSizes -join ',') allPng=$allPng"
  Write-Measurement -Number '3' -Label 'ICO structure' -Passed $pass3 -Detail $detail3
}
catch {
  Write-Measurement -Number '3' -Label 'ICO structure' -Passed $false -Detail $_.Exception.Message
}

# (4) Transparency: alpha 0 at all four corners, at least one opaque pixel.
try {
  $bitmap = [System.Drawing.Bitmap]::FromFile($resolvedAppIconPath)
  try {
    $grid = Get-BitmapArgbBytes -Bitmap $bitmap
    $w = $bitmap.Width
    $h = $bitmap.Height
  }
  finally { $bitmap.Dispose() }
  $stride = $grid.Stride
  $bytes = $grid.Bytes
  $topLeft = 0 * $stride + 0 * 4
  $topRight = 0 * $stride + ($w - 1) * 4
  $bottomLeft = ($h - 1) * $stride + 0 * 4
  $bottomRight = ($h - 1) * $stride + ($w - 1) * 4
  $cornerOffsets = @($topLeft, $topRight, $bottomLeft, $bottomRight)
  $cornersTransparent = -not ($cornerOffsets | Where-Object { $bytes[$_ + 3] -ne 0 })
  $opaqueFound = $false
  for ($i = 3; $i -lt $bytes.Length; $i += 4) {
    if ($bytes[$i] -eq 255) { $opaqueFound = $true; break }
  }
  $pass4 = $cornersTransparent -and $opaqueFound
  Write-Measurement -Number '4' -Label 'transparency' -Passed $pass4 -Detail "cornersAlpha0=$cornersTransparent opaquePixelFound=$opaqueFound"
}
catch {
  Write-Measurement -Number '4' -Label 'transparency' -Passed $false -Detail $_.Exception.Message
}

# (5) Simplified glyph direct measurement, independent of compose-ico.ps1's own encoding call.
try {
  $m5Out = Join-Path $scratchFull 'm5-render.png'
  & $systemPowerShellPath -ExecutionPolicy Bypass -File $renderScript -SvgPath $smallSvgPath -OutPath $m5Out | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "render-icon.ps1 (icon-small) exited with code $LASTEXITCODE" }

  $smallBytes = [System.IO.File]::ReadAllBytes($m5Out)
  $smallStream = New-Object System.IO.MemoryStream(, $smallBytes)
  $smallLoaded = $null
  $smallSource = $null
  try {
    $smallLoaded = [System.Drawing.Bitmap]::FromStream($smallStream)
    $smallSource = $smallLoaded.Clone(
      (New-Object System.Drawing.Rectangle(0, 0, $smallLoaded.Width, $smallLoaded.Height)),
      [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  }
  finally {
    if ($null -ne $smallLoaded) { $smallLoaded.Dispose() }
    $smallStream.Dispose()
  }

  $icoBytesForM5 = [System.IO.File]::ReadAllBytes($resolvedIcoPath)
  $parsedForM5 = ConvertFrom-IcoBytes -Bytes $icoBytesForM5

  $mismatchSizes = @()
  try {
    foreach ($size in @(16, 24, 32)) {
      $scaled = ConvertTo-MultiStageDownscale -Source $smallSource -TargetSize $size
      try {
        $expectedBytes = ConvertTo-PngBytes -Bitmap $scaled
      }
      finally { $scaled.Dispose() }
      $sizeMatches = @($parsedForM5.Entries | Where-Object { $_.Width -eq $size -and $_.Height -eq $size })
      if ($sizeMatches.Count -ne 1) {
        $mismatchSizes += "$size(missing)"
        continue
      }
      if (-not (Test-BytesEqual -Left $expectedBytes -Right $sizeMatches[0].Data)) {
        $mismatchSizes += "$size"
      }
    }
  }
  finally { $smallSource.Dispose() }

  $pass5 = ($mismatchSizes.Count -eq 0)
  $detail5 = if ($pass5) { '16/24/32 byte-identical' } else { "mismatched: $($mismatchSizes -join ', ')" }
  Write-Measurement -Number '5' -Label 'simplified glyph direct measurement' -Passed $pass5 -Detail $detail5
}
catch {
  Write-Measurement -Number '5' -Label 'simplified glyph direct measurement' -Passed $false -Detail $_.Exception.Message
}

# (6) Master pixel spot-check.
# Frozen from the current master: a deliberate artwork change must update these expectations.
try {
  $spotPoints = @(
    @{ X = 160; Y = 57; A = 0; R = 0; G = 0; B = 0; Opaque = $false },
    @{ X = 160; Y = 158; A = 0; R = 0; G = 0; B = 0; Opaque = $false },
    @{ X = 672; Y = 240; A = 255; R = 0x74; G = 0xc7; B = 0xec; Opaque = $true },
    @{ X = 693; Y = 269; A = 255; R = 0x74; G = 0xc7; B = 0xec; Opaque = $true },
    @{ X = 168; Y = 344; A = 255; R = 0x5f; G = 0xb9; B = 0xe8; Opaque = $true }
  )
  $pass6 = $false
  $detail6 = ''
  $spotBitmap = [System.Drawing.Bitmap]::FromFile($resolvedAppIconPath)
  try {
    if ($spotBitmap.Width -ne 1024 -or $spotBitmap.Height -ne 1024) {
      $detail6 = "expected a 1024x1024 master, got $($spotBitmap.Width)x$($spotBitmap.Height); sample coordinates are only defined for 1024x1024"
    }
    else {
      $spotMismatches = @()
      foreach ($spot in $spotPoints) {
        $spotResult = Test-SpotPixel -Bitmap $spotBitmap -X $spot.X -Y $spot.Y -ExpectedA $spot.A -ExpectedR $spot.R -ExpectedG $spot.G -ExpectedB $spot.B -Opaque $spot.Opaque
        if (-not $spotResult.Passed) { $spotMismatches += $spotResult.Detail }
      }
      $pass6 = ($spotMismatches.Count -eq 0)
      $detail6 = if ($pass6) { "$($spotPoints.Count) sample points match" } else { $spotMismatches -join '; ' }
    }
  }
  finally { $spotBitmap.Dispose() }
  Write-Measurement -Number '6' -Label 'master pixel spot-check' -Passed $pass6 -Detail $detail6
}
catch {
  Write-Measurement -Number '6' -Label 'master pixel spot-check' -Passed $false -Detail $_.Exception.Message
}

if ($failures -gt 0) {
  Write-Output "verify-icon: $failures measurement(s) failed"
  exit 1
}
Write-Output 'verify-icon: all measurements passed'
