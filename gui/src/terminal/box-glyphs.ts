/**
 * Box drawing (U+2500-U+257F) and block elements (U+2580-U+259F), described
 * as shapes in the cell so the renderer draws them to the cell's size rather
 * than with the font: lines then meet across cells, as xterm.js's custom
 * glyphs do.
 *
 * Coordinates are fractions of the cell: (0, 0) its top-left, (1, 1) its
 * bottom-right. Offsets (`dx`, `dy`) are in light stroke widths, which is
 * how the two strokes of a double line keep their spacing at any cell size.
 */

/** How thick a line is drawn. */
export type GlyphWeight = 'light' | 'heavy';

/** One shape of a glyph. */
export type GlyphShape =
  | { kind: 'line'; x1: number; y1: number; x2: number; y2: number; weight: GlyphWeight }
  | { kind: 'rect'; x: number; y: number; w: number; h: number }
  | {
      /** A light stroke of a double line, moved off its fractional position. */
      kind: 'offset_line';
      x1: number;
      y1: number;
      x2: number;
      y2: number;
      dx1: number;
      dy1: number;
      dx2: number;
      dy2: number;
    }
  | {
      /** A quadratic curve through control point (cx, cy). */
      kind: 'curve';
      x1: number;
      y1: number;
      cx: number;
      cy: number;
      x2: number;
      y2: number;
    }
  | {
      /** The whole cell in the foreground color at `density` opacity. */
      kind: 'shade';
      density: number;
    };

const FIRST = 0x2500;
const LAST = 0x259f;

/** One arm from the centre to an edge: none, light, heavy or double. */
type Arm = '0' | 'L' | 'H' | 'D';

/**
 * The arms of U+2500-U+257F in up, right, down, left order; `*` marks a
 * character drawn by {@link specialShapes} instead.
 */
const ARMS: readonly string[] = (
  '0L0L 0H0H L0L0 H0H0 **** **** **** **** **** **** **** **** 0LL0 0HL0 0LH0 0HH0 ' +
  '00LL 00LH 00HL 00HH LL00 LH00 HL00 HH00 L00L L00H H00L H00H LLL0 LHL0 HLL0 LLH0 ' +
  'HLH0 HHL0 LHH0 HHH0 L0LL L0LH H0LL L0HL H0HL H0LH L0HH H0HH 0LLL 0LLH 0HLL 0HLH ' +
  '0LHL 0LHH 0HHL 0HHH LL0L LL0H LH0L LH0H HL0L HL0H HH0L HH0H LLLL LLLH LHLL LHLH ' +
  'HLLL LLHL HLHL HLLH HHLL LLHH LHHL HHLH LHHH HLHH HHHL HHHH **** **** **** **** ' +
  '0D0D D0D0 0DL0 0LD0 0DD0 00LD 00DL 00DD LD00 DL00 DD00 L00D D00L D00D LDL0 DLD0 ' +
  'DDD0 L0LD D0DL D0DD 0DLD 0LDL 0DDD LD0D DL0L DD0D LDLD DLDL DDDD **** **** **** ' +
  '**** **** **** **** 000L L000 0L00 00L0 000H H000 0H00 00H0 0H0L L0H0 0L0H H0L0'
).split(' ');

/**
 * The shapes `text` is drawn with, or `null` when it is not one character of
 * U+2500-U+259F and the font draws it.
 */
export function boxGlyphShapes(text: string): GlyphShape[] | null {
  const chars = Array.from(text);
  const char = chars[0];
  if (chars.length !== 1 || char === undefined) return null;
  const code = char.codePointAt(0) ?? 0;
  if (code < FIRST || code > LAST) return null;
  const arms = ARMS[code - FIRST];
  if (arms !== undefined && arms !== '****') {
    return armShapes(arms);
  }
  return specialShapes(code);
}

function weightOf(arm: Arm): GlyphWeight {
  return arm === 'H' ? 'heavy' : 'light';
}

function line(x1: number, y1: number, x2: number, y2: number, weight: GlyphWeight): GlyphShape {
  return { kind: 'line', x1, y1, x2, y2, weight };
}

function rect(x: number, y: number, w: number, h: number): GlyphShape {
  return { kind: 'rect', x, y, w, h };
}

function armAt(arms: string, index: number): Arm {
  const arm = arms[index];
  return arm === 'L' || arm === 'H' || arm === 'D' ? arm : '0';
}

/**
 * The lines of a character made of arms. Along one axis, two equal single
 * arms are one line; a single arm that meets a double line on the other axis
 * stops at its near stroke when that line goes through, and at its far
 * stroke when it turns; the strokes of a double arm stop where the
 * perpendicular double line's strokes cross them.
 */
function armShapes(arms: string): GlyphShape[] {
  const [up, right, down, left] = [0, 1, 2, 3].map((index) => armAt(arms, index)) as [
    Arm,
    Arm,
    Arm,
    Arm,
  ];
  return [
    ...axisShapes('horizontal', left, right, up, down),
    ...axisShapes('vertical', up, down, left, right),
  ];
}

/**
 * The lines along one axis. `before` is the left (or up) arm, `after` the
 * right (or down) one; `sideA` / `sideB` are the arms across the axis on the
 * side of the negative / positive offset.
 */
function axisShapes(
  axis: 'horizontal' | 'vertical',
  before: Arm,
  after: Arm,
  sideA: Arm,
  sideB: Arm
): GlyphShape[] {
  const crossDouble = sideA === 'D' || sideB === 'D';
  const crossThrough = sideA !== '0' && sideB !== '0';
  const place = (
    from: number,
    to: number,
    fromOffset: number,
    toOffset: number,
    across: number,
    weight: GlyphWeight
  ): GlyphShape => {
    if (fromOffset === 0 && toOffset === 0 && across === 0) {
      return axis === 'horizontal'
        ? line(from, 0.5, to, 0.5, weight)
        : line(0.5, from, 0.5, to, weight);
    }
    return axis === 'horizontal'
      ? {
          kind: 'offset_line',
          x1: from,
          y1: 0.5,
          x2: to,
          y2: 0.5,
          dx1: fromOffset,
          dy1: across,
          dx2: toOffset,
          dy2: across,
        }
      : {
          kind: 'offset_line',
          x1: 0.5,
          y1: from,
          x2: 0.5,
          y2: to,
          dx1: across,
          dy1: fromOffset,
          dx2: across,
          dy2: toOffset,
        };
  };

  const shapes: GlyphShape[] = [];
  if (before === 'D' || after === 'D') {
    for (const side of [-1, 1]) {
      const sideArm = side < 0 ? sideA : sideB;
      // Where a stroke stops at the centre: inside the crossing double line
      // on its own side, outside it on the other.
      const stop = crossDouble ? (sideArm === 'D' ? 1 : -1) : 0;
      if (before === 'D' && after === 'D' && !crossDouble) {
        shapes.push(place(0, 1, 0, 0, side, 'light'));
        continue;
      }
      if (before === 'D') shapes.push(place(0, 0.5, 0, -stop, side, 'light'));
      if (after === 'D') shapes.push(place(0.5, 1, stop, 0, side, 'light'));
    }
    return shapes;
  }
  if (before !== '0' && before === after) {
    return [place(0, 1, 0, 0, 0, weightOf(before))];
  }
  // A single arm against a double line across the axis.
  const stop = crossDouble ? (crossThrough ? 1 : -1) : 0;
  if (before !== '0') shapes.push(place(0, 0.5, 0, -stop, 0, weightOf(before)));
  if (after !== '0') shapes.push(place(0.5, 1, stop, 0, 0, weightOf(after)));
  return shapes;
}

/** Dashed lines, arcs, diagonals and the block elements. */
function specialShapes(code: number): GlyphShape[] {
  switch (code) {
    case 0x2504:
      return dashes('horizontal', 3, 'light');
    case 0x2505:
      return dashes('horizontal', 3, 'heavy');
    case 0x2506:
      return dashes('vertical', 3, 'light');
    case 0x2507:
      return dashes('vertical', 3, 'heavy');
    case 0x2508:
      return dashes('horizontal', 4, 'light');
    case 0x2509:
      return dashes('horizontal', 4, 'heavy');
    case 0x250a:
      return dashes('vertical', 4, 'light');
    case 0x250b:
      return dashes('vertical', 4, 'heavy');
    case 0x254c:
      return dashes('horizontal', 2, 'light');
    case 0x254d:
      return dashes('horizontal', 2, 'heavy');
    case 0x254e:
      return dashes('vertical', 2, 'light');
    case 0x254f:
      return dashes('vertical', 2, 'heavy');
    case 0x256d:
      return [arc(1, 0.5, 0.5, 1)];
    case 0x256e:
      return [arc(0, 0.5, 0.5, 1)];
    case 0x256f:
      return [arc(0, 0.5, 0.5, 0)];
    case 0x2570:
      return [arc(1, 0.5, 0.5, 0)];
    case 0x2571:
      return [line(1, 0, 0, 1, 'light')];
    case 0x2572:
      return [line(0, 0, 1, 1, 'light')];
    case 0x2573:
      return [line(1, 0, 0, 1, 'light'), line(0, 0, 1, 1, 'light')];
    default:
      return blockShapes(code);
  }
}

function dashes(axis: 'horizontal' | 'vertical', count: number, weight: GlyphWeight): GlyphShape[] {
  return Array.from({ length: count }, (_, index) => {
    const from = (index + 0.25) / count;
    const to = (index + 0.75) / count;
    return axis === 'horizontal'
      ? line(from, 0.5, to, 0.5, weight)
      : line(0.5, from, 0.5, to, weight);
  });
}

/** A rounded corner from edge point (x1, y1) to edge point (x2, y2) through the centre. */
function arc(x1: number, y1: number, x2: number, y2: number): GlyphShape {
  return { kind: 'curve', x1, y1, cx: 0.5, cy: 0.5, x2, y2 };
}

const EIGHTH = 1 / 8;

/** The quadrants U+2596-U+259F fill: upper left, upper right, lower left, lower right. */
const QUADRANTS: Readonly<Record<number, readonly [boolean, boolean, boolean, boolean]>> = {
  0x2596: [false, false, true, false],
  0x2597: [false, false, false, true],
  0x2598: [true, false, false, false],
  0x2599: [true, false, true, true],
  0x259a: [true, false, false, true],
  0x259b: [true, true, true, false],
  0x259c: [true, true, false, true],
  0x259d: [false, true, false, false],
  0x259e: [false, true, true, false],
  0x259f: [false, true, true, true],
};

function blockShapes(code: number): GlyphShape[] {
  if (code === 0x2580) return [rect(0, 0, 1, 0.5)];
  // Lower one eighth to the full block.
  if (code >= 0x2581 && code <= 0x2588) {
    const height = (code - 0x2580) * EIGHTH;
    return [rect(0, 1 - height, 1, height)];
  }
  // Left seven eighths down to left one eighth.
  if (code >= 0x2589 && code <= 0x258f) {
    return [rect(0, 0, (0x2590 - code) * EIGHTH, 1)];
  }
  if (code === 0x2590) return [rect(0.5, 0, 0.5, 1)];
  if (code >= 0x2591 && code <= 0x2593) {
    return [{ kind: 'shade', density: (code - 0x2590) * 0.25 }];
  }
  if (code === 0x2594) return [rect(0, 0, 1, EIGHTH)];
  if (code === 0x2595) return [rect(1 - EIGHTH, 0, EIGHTH, 1)];
  const quadrants = QUADRANTS[code];
  if (quadrants === undefined) return [];
  const corners: readonly (readonly [number, number])[] = [
    [0, 0],
    [0.5, 0],
    [0, 0.5],
    [0.5, 0.5],
  ];
  return corners.flatMap(([x, y], index) =>
    quadrants[index] === true ? [rect(x, y, 0.5, 0.5)] : []
  );
}
