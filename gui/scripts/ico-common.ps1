<#
.SYNOPSIS
Shared helper functions for render-icon.ps1, compose-ico.ps1, and
verify-icon.ps1 (3 consumers): ConvertTo-MultiStageDownscale, ConvertTo-PngBytes,
ConvertFrom-IcoBytes, ConvertTo-IcoBytes (GDI+/ICO), and Resolve-OutputPath,
Assert-OutputRequiredForNonDefaultInput (output-path resolution and
non-default-input validation) -- 6 functions total.

.DESCRIPTION
Defines functions only; nothing runs at the top level. Callers must run
`Add-Type -AssemblyName System.Drawing` before dot-sourcing this file.
#>

function ConvertTo-MultiStageDownscale {
  <#
  Downscales a bitmap in the fixed stage sequence 1024 -> 256 -> 64 -> TargetSize,
  skipping any stage that would not shrink the current bitmap. Each stage uses
  HighQualityBicubic interpolation with TileFlipXY wrap mode to avoid edge
  ringing, and produces a fresh Format32bppArgb bitmap. The returned bitmap is
  independent of Source; Source is never disposed by this function.
  #>
  param(
    [Parameter(Mandatory = $true)][System.Drawing.Bitmap]$Source,
    [Parameter(Mandatory = $true)][int]$TargetSize
  )
  if ($TargetSize -lt 1) {
    throw "ico-common: target size must be positive, found $TargetSize"
  }
  if ($TargetSize -ge $Source.Width -or $TargetSize -ge $Source.Height) {
    throw "ico-common: target size $TargetSize must be smaller than the source ($($Source.Width)x$($Source.Height))"
  }

  $current = $Source
  $ownsCurrent = $false
  foreach ($stageSize in @(256, 64, $TargetSize)) {
    if ($stageSize -ge $current.Width -or $stageSize -ge $current.Height) { continue }
    $next = New-Object System.Drawing.Bitmap($stageSize, $stageSize, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($next)
    try {
      $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
      $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
      $attributes = New-Object System.Drawing.Imaging.ImageAttributes
      $attributes.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
      $destRect = New-Object System.Drawing.Rectangle(0, 0, $stageSize, $stageSize)
      $graphics.DrawImage($current, $destRect, 0, 0, $current.Width, $current.Height, [System.Drawing.GraphicsUnit]::Pixel, $attributes)
    }
    finally { $graphics.Dispose() }
    if ($ownsCurrent) { $current.Dispose() }
    $current = $next
    $ownsCurrent = $true
  }
  if (-not $ownsCurrent) {
    throw "ico-common: no downscale stage executed for target size $TargetSize"
  }
  return $current
}

function ConvertTo-PngBytes {
  <#
  Encodes a bitmap to PNG bytes. This is the only call site of Bitmap.Save
  for PNG encoding, so every PNG byte comparison in this project goes
  through the same code path.
  #>
  param([Parameter(Mandatory = $true)][System.Drawing.Bitmap]$Bitmap)
  $stream = New-Object System.IO.MemoryStream
  try {
    $Bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    # The leading comma keeps PowerShell from unrolling the byte array on
    # return; without it the caller receives an Object[] of boxed bytes.
    return , $stream.ToArray()
  }
  finally { $stream.Dispose() }
}

function ConvertFrom-IcoBytes {
  <#
  Parses an ICO byte array into its ICONDIR entries. Unparseable input
  throws. ICONDIRENTRY bWidth/bHeight of 0 is interpreted as 256, per the
  ICO format convention.
  #>
  param([Parameter(Mandatory = $true)][byte[]]$Bytes)
  if ($Bytes.Length -lt 6) { throw 'ico-common: ICO data is too short for a header' }
  $reserved = [BitConverter]::ToUInt16($Bytes, 0)
  $imageType = [BitConverter]::ToUInt16($Bytes, 2)
  $count = [BitConverter]::ToUInt16($Bytes, 4)
  if ($reserved -ne 0 -or $imageType -ne 1) {
    throw "ico-common: not an ICO file (reserved=$reserved, type=$imageType)"
  }
  $tableEnd = 6 + ($count * 16)
  if ($Bytes.Length -lt $tableEnd) { throw 'ico-common: ICO entry table is truncated' }

  $entries = @()
  for ($i = 0; $i -lt $count; $i++) {
    $base = 6 + ($i * 16)
    $bWidth = $Bytes[$base]
    $bHeight = $Bytes[$base + 1]
    $bColorCount = $Bytes[$base + 2]
    $wPlanes = [BitConverter]::ToUInt16($Bytes, $base + 4)
    $wBitCount = [BitConverter]::ToUInt16($Bytes, $base + 6)
    $dwBytesInRes = [BitConverter]::ToUInt32($Bytes, $base + 8)
    $dwImageOffset = [BitConverter]::ToUInt32($Bytes, $base + 12)
    if (([long]$dwImageOffset + [long]$dwBytesInRes) -gt $Bytes.Length) {
      throw "ico-common: ICO entry $i image data is out of range"
    }
    $data = New-Object byte[] ([int]$dwBytesInRes)
    [Array]::Copy($Bytes, [int]$dwImageOffset, $data, 0, [int]$dwBytesInRes)
    $entries += , [pscustomobject]@{
      Width = if ($bWidth -eq 0) { 256 } else { [int]$bWidth }
      Height = if ($bHeight -eq 0) { 256 } else { [int]$bHeight }
      ColorCount = [int]$bColorCount
      Planes = [int]$wPlanes
      BitCount = [int]$wBitCount
      Data = $data
    }
  }
  return [pscustomobject]@{ Entries = $entries }
}

function ConvertTo-IcoBytes {
  <#
  Assembles ICONDIR entries into an ICO byte array, recomputing the entry
  table offsets from each entry's Data length. Entry order is preserved as
  given. Width/Height of 256 are written as the 0 byte encoding.
  #>
  param([Parameter(Mandatory = $true)][object[]]$Entries)
  if ($Entries.Count -eq 0) { throw 'ico-common: cannot assemble an ICO with no entries' }

  $dataOffset = 6 + ($Entries.Count * 16)
  $stream = New-Object System.IO.MemoryStream
  try {
    $writer = New-Object System.IO.BinaryWriter($stream)
    $writer.Write([UInt16]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]$Entries.Count)

    $offset = $dataOffset
    foreach ($entry in $Entries) {
      if ($entry.Width -lt 1 -or $entry.Width -gt 256 -or $entry.Height -lt 1 -or $entry.Height -gt 256) {
        throw "ico-common: entry dimensions out of range ($($entry.Width)x$($entry.Height))"
      }
      $writer.Write([byte]$(if ($entry.Width -eq 256) { 0 } else { $entry.Width }))
      $writer.Write([byte]$(if ($entry.Height -eq 256) { 0 } else { $entry.Height }))
      $writer.Write([byte]$entry.ColorCount)
      $writer.Write([byte]0)
      $writer.Write([UInt16]$entry.Planes)
      $writer.Write([UInt16]$entry.BitCount)
      $writer.Write([UInt32]$entry.Data.Length)
      $writer.Write([UInt32]$offset)
      $offset += $entry.Data.Length
    }
    foreach ($entry in $Entries) {
      $writer.Write([byte[]]$entry.Data)
    }
    $writer.Flush()
    # See the comma note in ConvertTo-PngBytes: without it this Byte[] would
    # be unrolled into an Object[] of boxed bytes on return.
    return , $stream.ToArray()
  }
  finally { $stream.Dispose() }
}

function Resolve-OutputPath {
  <#
  Resolves an -OutPath argument to an absolute path: a rooted path is passed
  through GetFullPath as-is; a non-rooted path is joined against the current
  working directory first.
  #>
  param([Parameter(Mandatory = $true)][string]$Path)
  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }
  return [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $Path))
}

function Assert-OutputRequiredForNonDefaultInput {
  <#
  Throws Message when InputPath resolves (via GetFullPath) to something other
  than DefaultInputPath and OutputSpecified is false. Callers evaluate
  $PSBoundParameters.ContainsKey('OutPath') themselves and pass the result as
  OutputSpecified; ContainsKey is not used to judge the input side here.
  #>
  param(
    [Parameter(Mandatory = $true)][string]$InputPath,
    [Parameter(Mandatory = $true)][string]$DefaultInputPath,
    [Parameter(Mandatory = $true)][bool]$OutputSpecified,
    [Parameter(Mandatory = $true)][string]$Message
  )
  $isNonDefault = [System.IO.Path]::GetFullPath($InputPath) -ne [System.IO.Path]::GetFullPath($DefaultInputPath)
  if ($isNonDefault -and -not $OutputSpecified) {
    throw $Message
  }
}
