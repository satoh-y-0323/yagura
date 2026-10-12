<#
.SYNOPSIS
Rebuilds the 16/24/32 entries of icon.ico from the simplified glyph SVG,
leaving every other entry byte-identical.

.DESCRIPTION
Renders icons/icon-small.svg through render-icon.ps1 (child process), downscales
the 1024px result to 16/24/32 with the shared multi-stage pipeline, encodes each
as a PNG-compressed 32bpp ICONDIRENTRY (wPlanes=1, wBitCount=32), and splices
those three entries into -SourceIco. All other entries are carried over with
their original bytes untouched. Parsing failures and a source that lacks
exactly one 16x16/24x24/32x32 entry are fatal; no output is written on failure.
The render-icon.ps1 child process is launched via the absolute path of
Windows PowerShell 5.1, not the pwsh resolved from PATH. The final write is
atomic: bytes land in a temporary file next to -OutPath before being swapped
into place.

.PARAMETER SourceIco
Source ICO to read the non-replaced entries from. Defaults to the tracked
gui/src-tauri/icons/icon.ico. Passing a non-default -SourceIco requires -OutPath.

.PARAMETER OutPath
Destination ICO. Defaults to the tracked gui/src-tauri/icons/icon.ico.
#>
[CmdletBinding()]
param(
  [string]$SourceIco,
  [string]$OutPath
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
. (Join-Path $PSScriptRoot 'ico-common.ps1')

$renderScript = Join-Path $PSScriptRoot 'render-icon.ps1'
$smallSvgPath = Join-Path $PSScriptRoot '..\src-tauri\icons\icon-small.svg'
$sourceIcoDefault = Join-Path $PSScriptRoot '..\src-tauri\icons\icon.ico'
$outPathDefault = Join-Path $PSScriptRoot '..\src-tauri\icons\icon.ico'
$replacedSizes = @(16, 24, 32)
$systemPowerShellPath = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'

$sourceIcoRaw = if ($PSBoundParameters.ContainsKey('SourceIco')) { $SourceIco } else { $sourceIcoDefault }
if (-not (Test-Path -LiteralPath $sourceIcoRaw)) {
  throw "compose-ico: source ICO not found: $sourceIcoRaw"
}
$resolvedSourceIco = (Resolve-Path -LiteralPath $sourceIcoRaw).Path
Assert-OutputRequiredForNonDefaultInput -InputPath $resolvedSourceIco -DefaultInputPath $sourceIcoDefault -OutputSpecified $PSBoundParameters.ContainsKey('OutPath') -Message 'compose-ico: -OutPath is required when -SourceIco is not the default icon.ico'

$resolvedOutRaw = if ($PSBoundParameters.ContainsKey('OutPath')) { $OutPath } else { $outPathDefault }
$resolvedOut = Resolve-OutputPath -Path $resolvedOutRaw

$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ('compose-ico-' + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
try {
  $smallPngPath = Join-Path $tempDir 'icon-small.png'
  & $systemPowerShellPath -ExecutionPolicy Bypass -File $renderScript -SvgPath $smallSvgPath -OutPath $smallPngPath | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "compose-ico: render-icon.ps1 failed with exit code $LASTEXITCODE"
  }

  $smallBytes = [System.IO.File]::ReadAllBytes($smallPngPath)
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

  $composedBySize = @{}
  try {
    foreach ($size in $replacedSizes) {
      $scaled = ConvertTo-MultiStageDownscale -Source $smallSource -TargetSize $size
      try {
        $pngBytes = ConvertTo-PngBytes -Bitmap $scaled
      }
      finally { $scaled.Dispose() }
      $composedBySize[$size] = [pscustomobject]@{
        Width = $size
        Height = $size
        ColorCount = 0
        Planes = 1
        BitCount = 32
        Data = $pngBytes
      }
    }
  }
  finally { $smallSource.Dispose() }

  $sourceBytes = [System.IO.File]::ReadAllBytes($resolvedSourceIco)
  $parsed = ConvertFrom-IcoBytes -Bytes $sourceBytes

  foreach ($size in $replacedSizes) {
    $sizeMatches = @($parsed.Entries | Where-Object { $_.Width -eq $size -and $_.Height -eq $size })
    if ($sizeMatches.Count -ne 1) {
      throw "compose-ico: source ICO must have exactly one ${size}x${size} entry, found $($sizeMatches.Count)"
    }
  }

  $finalEntries = @()
  foreach ($entry in $parsed.Entries) {
    if (($entry.Width -eq $entry.Height) -and $composedBySize.ContainsKey($entry.Width)) {
      $finalEntries += , $composedBySize[$entry.Width]
    }
    else {
      $finalEntries += , $entry
    }
  }

  $icoBytes = ConvertTo-IcoBytes -Entries $finalEntries
  $outDirectory = Split-Path -Parent $resolvedOut
  $outLeaf = Split-Path -Leaf $resolvedOut
  $tempOutPath = Join-Path $outDirectory ($outLeaf + '.tmp-' + [System.IO.Path]::GetRandomFileName())
  $backupOutPath = $null
  try {
    [System.IO.File]::WriteAllBytes($tempOutPath, $icoBytes)
    if (Test-Path -LiteralPath $resolvedOut) {
      # File.Replace's destinationBackupFileName throws ArgumentException on
      # .NET Framework (PowerShell 5.1) when passed $null, so a real backup
      # path is supplied and removed again once the swap has succeeded.
      $backupOutPath = Join-Path $outDirectory ($outLeaf + '.tmp-' + [System.IO.Path]::GetRandomFileName())
      [System.IO.File]::Replace($tempOutPath, $resolvedOut, $backupOutPath)
      Remove-Item -LiteralPath $backupOutPath -Force -ErrorAction SilentlyContinue
    }
    else {
      Move-Item -LiteralPath $tempOutPath -Destination $resolvedOut
    }
  }
  catch {
    Remove-Item -LiteralPath $tempOutPath -Force -ErrorAction SilentlyContinue
    if ($null -ne $backupOutPath) { Remove-Item -LiteralPath $backupOutPath -Force -ErrorAction SilentlyContinue }
    throw
  }
  Write-Output "compose-ico: wrote $resolvedOut ($($icoBytes.Length) bytes, $($finalEntries.Count) entries)"
}
finally {
  Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
