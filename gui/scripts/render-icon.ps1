<#
.SYNOPSIS
Renders an icon SVG (restricted subset v2) to a square PNG without anti-aliasing.

.DESCRIPTION
The bundled .NET has no SVG rasterizer, so the source SVG is held to a
restricted subset -- linearGradient/stop, path (M/L/l), rect, and circle --
and rasterized here with GDI+. Anything outside that subset is an error
rather than a silently dropped shape. Rendering is deterministic: the same
source SVG always yields byte-identical output. Dot-sources ico-common.ps1
to share its output-path resolution and validation guards.

.PARAMETER SvgPath
Source SVG. Defaults to gui/src-tauri/icons/icon.svg next to this script's
repository. An explicit relative path resolves against the current working
directory. Passing a non-default -SvgPath requires -OutPath.

.PARAMETER OutPath
Destination PNG. Defaults to gui/src-tauri/app-icon.png next to this
script's repository, so verification runs can render elsewhere without
touching the tracked asset.
#>
[CmdletBinding()]
param(
  [string]$SvgPath,
  [string]$OutPath
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
. (Join-Path $PSScriptRoot 'ico-common.ps1')

$OutputSize = 1024
$svgPathDefault = Join-Path $PSScriptRoot '..\src-tauri\icons\icon.svg'
$outPathDefault = Join-Path $PSScriptRoot '..\src-tauri\app-icon.png'

$svgPathRaw = if ($PSBoundParameters.ContainsKey('SvgPath')) { $SvgPath } else { $svgPathDefault }
if (-not (Test-Path -LiteralPath $svgPathRaw)) {
  throw "render-icon: SVG source not found: $svgPathRaw"
}
$resolvedSvgPath = (Resolve-Path -LiteralPath $svgPathRaw).Path
Assert-OutputRequiredForNonDefaultInput -InputPath $resolvedSvgPath -DefaultInputPath $svgPathDefault -OutputSpecified $PSBoundParameters.ContainsKey('OutPath') -Message 'render-icon: -OutPath is required when -SvgPath is not the default icon.svg'

$svgAllowed = @('xmlns', 'viewBox', 'width', 'height')
$linearGradientAllowed = @('id', 'x1', 'y1', 'x2', 'y2')
$stopAllowed = @('offset', 'stop-color')
$pathAllowed = @('d', 'fill', 'stroke', 'stroke-width', 'stroke-linecap', 'stroke-linejoin')
$rectAllowed = @('x', 'y', 'width', 'height', 'fill', 'rx', 'stroke', 'stroke-width')
$circleAllowed = @('cx', 'cy', 'r', 'fill')

function Get-Attributes {
  param([string]$Tag, [string[]]$Allowed, [string]$Element)
  $attributes = @{}
  foreach ($match in [regex]::Matches($Tag, '([A-Za-z:_][\w:.-]*)\s*=\s*"([^"]*)"')) {
    $name = $match.Groups[1].Value
    if ($Allowed -notcontains $name) {
      throw "render-icon: unsupported attribute '$name' on <$Element>; icon SVG must stay within the restricted subset"
    }
    $attributes[$name] = $match.Groups[2].Value
  }
  return $attributes
}

function Assert-RequiredAttributes {
  param([hashtable]$Attributes, [string[]]$Required, [string]$Element)
  foreach ($name in $Required) {
    if (-not $Attributes.ContainsKey($name)) {
      throw "render-icon: <$Element> is missing the '$name' attribute"
    }
  }
}

function ConvertTo-Color {
  param([string]$Fill)
  if ($Fill -notmatch '^#[0-9A-Fa-f]{6}$') {
    throw "render-icon: unsupported color '$Fill'; only #rrggbb is understood"
  }
  $r = [Convert]::ToInt32($Fill.Substring(1, 2), 16)
  $g = [Convert]::ToInt32($Fill.Substring(3, 2), 16)
  $b = [Convert]::ToInt32($Fill.Substring(5, 2), 16)
  return [System.Drawing.Color]::FromArgb(255, $r, $g, $b)
}

function ConvertTo-Double {
  param([string]$Value, [string]$Context)
  $result = 0.0
  if (-not [double]::TryParse($Value, [System.Globalization.NumberStyles]::Float, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$result)) {
    throw "render-icon: invalid numeric value '$Value' ($Context)"
  }
  return $result
}

function ConvertFrom-PathData {
  param([string]$D)
  $normalized = ($D -replace ',', ' ')
  $normalized = $normalized -replace '(?<=[MLl])(?=[0-9.\-])', ' '
  $normalized = $normalized -replace '(?<=[0-9.])-', ' -'
  $tokens = @($normalized.Trim() -split '\s+' | Where-Object { $_ -ne '' })
  if ($tokens.Count -eq 0) { throw 'render-icon: <path> has an empty d attribute' }
  $points = New-Object System.Collections.ArrayList
  $command = $null
  $index = 0
  while ($index -lt $tokens.Count) {
    $token = $tokens[$index]
    if ($token -match '^[A-Za-z]') {
      # Case-sensitive throughout: 'l' (relative) and 'L' (absolute) are
      # distinct commands, and PowerShell's default comparisons are not.
      if ($token -cnotmatch '^[MLl]$') {
        throw "render-icon: unsupported path command '$token'"
      }
      $command = $token
      $index++
      if ($points.Count -eq 0 -and $command -cne 'M') {
        throw "render-icon: path d must start with 'M'"
      }
      continue
    }
    if ($null -eq $command) {
      throw "render-icon: path d must start with 'M'"
    }
    if ($index + 1 -ge $tokens.Count) {
      throw 'render-icon: path d has an incomplete coordinate pair'
    }
    $x = ConvertTo-Double -Value $tokens[$index] -Context 'path d'
    $y = ConvertTo-Double -Value $tokens[$index + 1] -Context 'path d'
    $index += 2
    if ($command -ceq 'M') {
      [void]$points.Add(@($x, $y))
      $command = 'L'
    }
    elseif ($command -ceq 'L') {
      [void]$points.Add(@($x, $y))
    }
    else {
      $last = $points[$points.Count - 1]
      [void]$points.Add(@(($last[0] + $x), ($last[1] + $y)))
    }
  }
  # Wrapped so a lone-point result is not unrolled by the pipeline on return.
  return [pscustomobject]@{ Points = $points }
}

function Resolve-Stroke {
  param([string]$Value, [hashtable]$Gradients)
  if ($Value -match '^#[0-9A-Fa-f]{6}$') {
    return @{ Kind = 'solid'; Color = (ConvertTo-Color -Fill $Value) }
  }
  if ($Value -match '^url\(#([A-Za-z_][\w:.-]*)\)$') {
    $id = $Matches[1]
    if (-not $Gradients.ContainsKey($id)) {
      throw "render-icon: <path> stroke references unknown gradient '#$id'"
    }
    return @{ Kind = 'gradient'; Id = $id }
  }
  throw "render-icon: unsupported stroke '$Value'; expected #rrggbb or url(#id)"
}

function New-RoundedRectPath {
  param([double]$X, [double]$Y, [double]$Width, [double]$Height, [double]$Radius)
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $Radius * 2
  $path.AddArc([float]$X, [float]$Y, [float]$d, [float]$d, 180, 90)
  $path.AddArc([float]($X + $Width - $d), [float]$Y, [float]$d, [float]$d, 270, 90)
  $path.AddArc([float]($X + $Width - $d), [float]($Y + $Height - $d), [float]$d, [float]$d, 0, 90)
  $path.AddArc([float]$X, [float]($Y + $Height - $d), [float]$d, [float]$d, 90, 90)
  $path.CloseFigure()
  return $path
}

function New-PadLinearGradientBrush {
  param(
    [double]$MinX, [double]$MinY, [double]$MaxX, [double]$MaxY,
    [double]$StrokeWidth,
    [System.Drawing.Color]$StartColor,
    [System.Drawing.Color]$EndColor
  )
  if ($StrokeWidth -le 0) {
    throw 'render-icon: a gradient stroke requires stroke-width > 0'
  }
  $axisX = $MaxX - $MinX
  $axisY = $MaxY - $MinY
  $axisLenSq = $axisX * $axisX + $axisY * $axisY
  $half = $StrokeWidth / 2.0
  $corners = @(
    @(($MinX - $half), ($MinY - $half)),
    @(($MaxX + $half), ($MinY - $half)),
    @(($MinX - $half), ($MaxY + $half)),
    @(($MaxX + $half), ($MaxY + $half))
  )
  $ts = @()
  foreach ($corner in $corners) {
    $dx = $corner[0] - $MinX
    $dy = $corner[1] - $MinY
    $ts += (($dx * $axisX) + ($dy * $axisY)) / $axisLenSq
  }
  $tMin = ($ts | Measure-Object -Minimum).Minimum
  $tMax = ($ts | Measure-Object -Maximum).Maximum
  if (-not ($tMin -lt 0 -and $tMax -gt 1)) {
    throw 'render-icon: gradient stroke does not extend past its reference bounding box'
  }
  $p0Prime = New-Object System.Drawing.PointF([float]($MinX + $tMin * $axisX), [float]($MinY + $tMin * $axisY))
  $p1Prime = New-Object System.Drawing.PointF([float]($MinX + $tMax * $axisX), [float]($MinY + $tMax * $axisY))
  $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($p0Prime, $p1Prime, $StartColor, $EndColor)
  $pos1 = (0 - $tMin) / ($tMax - $tMin)
  $pos2 = (1 - $tMin) / ($tMax - $tMin)
  $blend = New-Object System.Drawing.Drawing2D.ColorBlend(4)
  $blend.Colors = [System.Drawing.Color[]]@($StartColor, $StartColor, $EndColor, $EndColor)
  $blend.Positions = [float[]]@(0.0, [float]$pos1, [float]$pos2, 1.0)
  $brush.InterpolationColors = $blend
  return $brush
}

$svgText = [System.IO.File]::ReadAllText($resolvedSvgPath)
$svgText = [regex]::Replace($svgText, '(?s)<!--.*?-->', '')

$scale = $null
$gradients = @{}
$shapes = @()
$currentGradient = $null

foreach ($match in [regex]::Matches($svgText, '(?s)<[^>]*>')) {
  $tag = $match.Value

  if ($tag -match '^</\s*svg\s*>$') { continue }
  if ($tag -match '^</\s*linearGradient\s*>$') {
    if ($null -eq $currentGradient) { throw 'render-icon: unmatched </linearGradient>' }
    if ($null -eq $currentGradient.Start -or $null -eq $currentGradient.End) {
      throw "render-icon: <linearGradient id='$($currentGradient.Id)'> is missing a stop at offset 0 or 1"
    }
    $gradients[$currentGradient.Id] = @{ Start = $currentGradient.Start; End = $currentGradient.End }
    $currentGradient = $null
    continue
  }
  if ($tag -match '^</') {
    $name = if ($tag -match '^</\s*([A-Za-z][\w:.-]*)') { $Matches[1] } else { $tag }
    throw "render-icon: unsupported closing tag '</$name>'"
  }

  $isSelfClosing = [bool]($tag -match '/>\s*$')

  if ($tag -match '^<\s*svg\b') {
    if ($isSelfClosing) { throw 'render-icon: <svg> must not be self-closing' }
    $attributes = Get-Attributes -Tag $tag -Allowed $svgAllowed -Element 'svg'
    Assert-RequiredAttributes -Attributes $attributes -Required $svgAllowed -Element 'svg'
    $viewBox = $attributes['viewBox']
    if ($viewBox -notmatch '^\s*0\s+0\s+(\d+)\s+(\d+)\s*$') {
      throw "render-icon: viewBox must be '0 0 <size> <size>', found '$viewBox'"
    }
    if ($Matches[1] -ne $Matches[2]) {
      throw "render-icon: viewBox must be square, found '$viewBox'"
    }
    $scale = $OutputSize / [double]$Matches[1]
    continue
  }

  if ($null -eq $scale) {
    throw 'render-icon: <svg> root element must appear first'
  }

  if ($tag -match '^<\s*linearGradient\b') {
    if ($isSelfClosing) { throw 'render-icon: <linearGradient> must not be self-closing' }
    if ($null -ne $currentGradient) { throw 'render-icon: nested <linearGradient> is not supported' }
    $attributes = Get-Attributes -Tag $tag -Allowed $linearGradientAllowed -Element 'linearGradient'
    Assert-RequiredAttributes -Attributes $attributes -Required $linearGradientAllowed -Element 'linearGradient'
    $x1 = ConvertTo-Double -Value $attributes['x1'] -Context 'linearGradient x1'
    $y1 = ConvertTo-Double -Value $attributes['y1'] -Context 'linearGradient y1'
    $x2 = ConvertTo-Double -Value $attributes['x2'] -Context 'linearGradient x2'
    $y2 = ConvertTo-Double -Value $attributes['y2'] -Context 'linearGradient y2'
    if ($x1 -ne 0 -or $y1 -ne 0 -or $x2 -ne 1 -or $y2 -ne 1) {
      throw "render-icon: <linearGradient> x1/y1/x2/y2 must be '0,0,1,1', found '$x1,$y1,$x2,$y2'"
    }
    $currentGradient = [pscustomobject]@{ Id = $attributes['id']; Start = $null; End = $null }
    continue
  }

  if ($tag -match '^<\s*stop\b') {
    if (-not $isSelfClosing) { throw 'render-icon: <stop> must be self-closing' }
    if ($null -eq $currentGradient) { throw 'render-icon: <stop> outside <linearGradient>' }
    $attributes = Get-Attributes -Tag $tag -Allowed $stopAllowed -Element 'stop'
    Assert-RequiredAttributes -Attributes $attributes -Required $stopAllowed -Element 'stop'
    $offset = ConvertTo-Double -Value $attributes['offset'] -Context 'stop offset'
    $color = ConvertTo-Color -Fill $attributes['stop-color']
    if ($offset -eq 0) { $currentGradient.Start = $color }
    elseif ($offset -eq 1) { $currentGradient.End = $color }
    else { throw "render-icon: <stop> offset must be 0 or 1, found '$offset'" }
    continue
  }

  if ($null -ne $currentGradient) {
    throw 'render-icon: only <stop> is allowed inside <linearGradient>'
  }

  if ($tag -match '^<\s*rect\b') {
    if (-not $isSelfClosing) { throw 'render-icon: <rect> must be self-closing' }
    $attributes = Get-Attributes -Tag $tag -Allowed $rectAllowed -Element 'rect'
    Assert-RequiredAttributes -Attributes $attributes -Required @('x', 'y', 'width', 'height', 'fill') -Element 'rect'
    if ($attributes.ContainsKey('stroke-width') -and -not $attributes.ContainsKey('stroke')) {
      throw "render-icon: <rect> has 'stroke-width' without 'stroke'"
    }
    $shapes += , @{
      Type = 'rect'
      X = (ConvertTo-Double -Value $attributes['x'] -Context 'rect x') * $scale
      Y = (ConvertTo-Double -Value $attributes['y'] -Context 'rect y') * $scale
      Width = (ConvertTo-Double -Value $attributes['width'] -Context 'rect width') * $scale
      Height = (ConvertTo-Double -Value $attributes['height'] -Context 'rect height') * $scale
      Fill = (ConvertTo-Color -Fill $attributes['fill'])
      Rx = if ($attributes.ContainsKey('rx')) { (ConvertTo-Double -Value $attributes['rx'] -Context 'rect rx') * $scale } else { $null }
      Stroke = if ($attributes.ContainsKey('stroke')) { ConvertTo-Color -Fill $attributes['stroke'] } else { $null }
      StrokeWidth = if ($attributes.ContainsKey('stroke-width')) { (ConvertTo-Double -Value $attributes['stroke-width'] -Context 'rect stroke-width') * $scale } else { $null }
    }
    continue
  }

  if ($tag -match '^<\s*circle\b') {
    if (-not $isSelfClosing) { throw 'render-icon: <circle> must be self-closing' }
    $attributes = Get-Attributes -Tag $tag -Allowed $circleAllowed -Element 'circle'
    Assert-RequiredAttributes -Attributes $attributes -Required $circleAllowed -Element 'circle'
    $shapes += , @{
      Type = 'circle'
      Cx = (ConvertTo-Double -Value $attributes['cx'] -Context 'circle cx') * $scale
      Cy = (ConvertTo-Double -Value $attributes['cy'] -Context 'circle cy') * $scale
      R = (ConvertTo-Double -Value $attributes['r'] -Context 'circle r') * $scale
      Fill = (ConvertTo-Color -Fill $attributes['fill'])
    }
    continue
  }

  if ($tag -match '^<\s*path\b') {
    if (-not $isSelfClosing) { throw 'render-icon: <path> must be self-closing' }
    $attributes = Get-Attributes -Tag $tag -Allowed $pathAllowed -Element 'path'
    Assert-RequiredAttributes -Attributes $attributes -Required $pathAllowed -Element 'path'
    if ($attributes['fill'] -ne 'none') {
      throw "render-icon: <path> fill must be 'none', found '$($attributes['fill'])'"
    }
    if ($attributes['stroke-linecap'] -ne 'round') {
      throw "render-icon: <path> stroke-linecap must be 'round', found '$($attributes['stroke-linecap'])'"
    }
    if ($attributes['stroke-linejoin'] -ne 'round') {
      throw "render-icon: <path> stroke-linejoin must be 'round', found '$($attributes['stroke-linejoin'])'"
    }
    $rawPoints = (ConvertFrom-PathData -D $attributes['d']).Points
    if ($rawPoints.Count -lt 2) {
      throw 'render-icon: <path> d must describe at least two points'
    }
    $scaledPoints = @()
    foreach ($p in $rawPoints) { $scaledPoints += , @(($p[0] * $scale), ($p[1] * $scale)) }
    $strokeWidth = (ConvertTo-Double -Value $attributes['stroke-width'] -Context 'path stroke-width') * $scale
    $stroke = Resolve-Stroke -Value $attributes['stroke'] -Gradients $gradients
    $shapes += , @{
      Type = 'path'
      Points = $scaledPoints
      StrokeKind = $stroke.Kind
      StrokeColor = $stroke.Color
      StrokeGradientId = $stroke.Id
      StrokeWidth = $strokeWidth
    }
    continue
  }

  $name = if ($tag -match '^<\s*([A-Za-z][\w:.-]*)') { $Matches[1] } else { $tag }
  throw "render-icon: unsupported element '<$name>'; icon SVG must stay within the restricted subset"
}

if ($null -ne $currentGradient) {
  throw "render-icon: <linearGradient id='$($currentGradient.Id)'> is missing its closing tag"
}
if ($null -eq $scale) { throw 'render-icon: no <svg> root element found' }
if ($shapes.Count -eq 0) { throw 'render-icon: SVG contains no shapes to draw' }

$bitmap = New-Object System.Drawing.Bitmap($OutputSize, $OutputSize, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
try {
  $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::None
  $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
  $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor

  foreach ($shape in $shapes) {
    switch ($shape.Type) {
      'rect' {
        if ($null -ne $shape.Rx) {
          $roundedPath = New-RoundedRectPath -X $shape.X -Y $shape.Y -Width $shape.Width -Height $shape.Height -Radius $shape.Rx
          $brush = New-Object System.Drawing.SolidBrush($shape.Fill)
          try { $graphics.FillPath($brush, $roundedPath) } finally { $brush.Dispose() }
          if ($null -ne $shape.Stroke) {
            $pen = New-Object System.Drawing.Pen($shape.Stroke, [float]$shape.StrokeWidth)
            try { $graphics.DrawPath($pen, $roundedPath) } finally { $pen.Dispose() }
          }
          $roundedPath.Dispose()
        }
        else {
          $brush = New-Object System.Drawing.SolidBrush($shape.Fill)
          try {
            $graphics.FillRectangle($brush, [float]$shape.X, [float]$shape.Y, [float]$shape.Width, [float]$shape.Height)
          } finally { $brush.Dispose() }
          if ($null -ne $shape.Stroke) {
            $pen = New-Object System.Drawing.Pen($shape.Stroke, [float]$shape.StrokeWidth)
            try {
              $graphics.DrawRectangle($pen, [float]$shape.X, [float]$shape.Y, [float]$shape.Width, [float]$shape.Height)
            } finally { $pen.Dispose() }
          }
        }
      }
      'circle' {
        $brush = New-Object System.Drawing.SolidBrush($shape.Fill)
        try {
          $graphics.FillEllipse($brush, [float]($shape.Cx - $shape.R), [float]($shape.Cy - $shape.R), [float](2 * $shape.R), [float](2 * $shape.R))
        } finally { $brush.Dispose() }
      }
      'path' {
        $points = [System.Drawing.PointF[]]@($shape.Points | ForEach-Object { New-Object System.Drawing.PointF([float]$_[0], [float]$_[1]) })
        # Logged at the draw site, not at parse time, so it describes the array DrawLines actually receives.
        $pointLog = ($points | ForEach-Object { '({0},{1})' -f $_.X, $_.Y }) -join ' '
        Write-Verbose "render-icon: DrawLines receives $($points.Length) point(s): $pointLog"
        $gradBrush = $null
        if ($shape.StrokeKind -eq 'solid') {
          $pen = New-Object System.Drawing.Pen($shape.StrokeColor, [float]$shape.StrokeWidth)
        }
        else {
          $xs = $shape.Points | ForEach-Object { $_[0] }
          $ys = $shape.Points | ForEach-Object { $_[1] }
          $minX = ($xs | Measure-Object -Minimum).Minimum
          $maxX = ($xs | Measure-Object -Maximum).Maximum
          $minY = ($ys | Measure-Object -Minimum).Minimum
          $maxY = ($ys | Measure-Object -Maximum).Maximum
          $gradient = $gradients[$shape.StrokeGradientId]
          $gradBrush = New-PadLinearGradientBrush -MinX $minX -MinY $minY -MaxX $maxX -MaxY $maxY -StrokeWidth $shape.StrokeWidth -StartColor $gradient.Start -EndColor $gradient.End
          $pen = New-Object System.Drawing.Pen($gradBrush, [float]$shape.StrokeWidth)
        }
        $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
        $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
        $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        try {
          $graphics.DrawLines($pen, $points)
        } finally {
          $pen.Dispose()
          if ($null -ne $gradBrush) { $gradBrush.Dispose() }
        }
      }
    }
  }
}
finally { $graphics.Dispose() }

$resolvedOutRaw = if ($PSBoundParameters.ContainsKey('OutPath')) { $OutPath } else { $outPathDefault }
$resolvedOut = Resolve-OutputPath -Path $resolvedOutRaw
$outDirectory = Split-Path -Parent $resolvedOut
if (-not (Test-Path -LiteralPath $outDirectory)) {
  New-Item -ItemType Directory -Path $outDirectory -Force | Out-Null
}
$bitmap.Save($resolvedOut, [System.Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()
Write-Output "render-icon: wrote $resolvedOut ($OutputSize x $OutputSize)"
