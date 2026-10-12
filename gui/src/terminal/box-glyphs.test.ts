import { describe, it, expect } from 'vitest';
import { boxGlyphShapes } from './box-glyphs';
import { shapeOps, type ShapeOp } from './paint-plan';

/** The filled pixels of `char` drawn in a `size` by `size` cell with 1-pixel light lines. */
function pixelsOf(char: string, size: number): boolean[][] {
  const shapes = boxGlyphShapes(char);
  if (shapes === null) throw new Error(`${char} is not drawn by the renderer`);
  const grid = Array.from({ length: size }, () => Array.from({ length: size }, () => false));
  for (const op of shapeOps(shapes, size, size, 1)) {
    if (op.kind !== 'fill') continue;
    for (let y = op.rect.y; y < op.rect.y + op.rect.h; y += 1) {
      for (let x = op.rect.x; x < op.rect.x + op.rect.w; x += 1) {
        const row = grid[y];
        if (row === undefined || x < 0 || x >= size) {
          throw new Error(`${char} draws outside its cell`);
        }
        row[x] = true;
      }
    }
  }
  return grid;
}

function pictureOf(char: string, size: number): string {
  const rows = pixelsOf(char, size).map((row) => row.map((on) => (on ? '#' : '.')).join(''));
  return `\n${rows.join('\n')}\n`;
}

function opsOf(char: string, size: number): ShapeOp[] {
  return shapeOps(boxGlyphShapes(char) ?? [], size, size, 1);
}

const SIZE = 16;
type Edge = 'top' | 'right' | 'bottom' | 'left';

/** The pixels along one edge of the cell, as a string of # and . */
function edgeOf(char: string, edge: Edge): string {
  const grid = pixelsOf(char, SIZE);
  const last = SIZE - 1;
  const cells = Array.from({ length: SIZE }, (_, i) => {
    switch (edge) {
      case 'top':
        return grid[0]?.[i];
      case 'bottom':
        return grid[last]?.[i];
      case 'left':
        return grid[i]?.[0];
      case 'right':
        return grid[i]?.[last];
    }
  });
  return cells.map((on) => (on === true ? '#' : '.')).join('');
}

const NOTHING = '.'.repeat(SIZE);

// Each box drawing character of U+2500-U+257F made of straight arms, with its
// arms (up, right, down, left; 0 none, L light, H heavy, D double) as the
// Unicode character names give them.
const UNICODE_ARMS = [
  '─ 0L0L',
  '━ 0H0H',
  '│ L0L0',
  '┃ H0H0',
  '┌ 0LL0',
  '┍ 0HL0',
  '┎ 0LH0',
  '┏ 0HH0',
  '┐ 00LL',
  '┑ 00LH',
  '┒ 00HL',
  '┓ 00HH',
  '└ LL00',
  '┕ LH00',
  '┖ HL00',
  '┗ HH00',
  '┘ L00L',
  '┙ L00H',
  '┚ H00L',
  '┛ H00H',
  '├ LLL0',
  '┝ LHL0',
  '┞ HLL0',
  '┟ LLH0',
  '┠ HLH0',
  '┡ HHL0',
  '┢ LHH0',
  '┣ HHH0',
  '┤ L0LL',
  '┥ L0LH',
  '┦ H0LL',
  '┧ L0HL',
  '┨ H0HL',
  '┩ H0LH',
  '┪ L0HH',
  '┫ H0HH',
  '┬ 0LLL',
  '┭ 0LLH',
  '┮ 0HLL',
  '┯ 0HLH',
  '┰ 0LHL',
  '┱ 0LHH',
  '┲ 0HHL',
  '┳ 0HHH',
  '┴ LL0L',
  '┵ LL0H',
  '┶ LH0L',
  '┷ LH0H',
  '┸ HL0L',
  '┹ HL0H',
  '┺ HH0L',
  '┻ HH0H',
  '┼ LLLL',
  '┽ LLLH',
  '┾ LHLL',
  '┿ LHLH',
  '╀ HLLL',
  '╁ LLHL',
  '╂ HLHL',
  '╃ HLLH',
  '╄ HHLL',
  '╅ LLHH',
  '╆ LHHL',
  '╇ HHLH',
  '╈ LHHH',
  '╉ HLHH',
  '╊ HHHL',
  '╋ HHHH',
  '═ 0D0D',
  '║ D0D0',
  '╒ 0DL0',
  '╓ 0LD0',
  '╔ 0DD0',
  '╕ 00LD',
  '╖ 00DL',
  '╗ 00DD',
  '╘ LD00',
  '╙ DL00',
  '╚ DD00',
  '╛ L00D',
  '╜ D00L',
  '╝ D00D',
  '╞ LDL0',
  '╟ DLD0',
  '╠ DDD0',
  '╡ L0LD',
  '╢ D0DL',
  '╣ D0DD',
  '╤ 0DLD',
  '╥ 0LDL',
  '╦ 0DDD',
  '╧ LD0D',
  '╨ DL0L',
  '╩ DD0D',
  '╪ LDLD',
  '╫ DLDL',
  '╬ DDDD',
  '╴ 000L',
  '╵ L000',
  '╶ 0L00',
  '╷ 00L0',
  '╸ 000H',
  '╹ H000',
  '╺ 0H00',
  '╻ 00H0',
  '╼ 0H0L',
  '╽ L0H0',
  '╾ 0L0H',
  '╿ H0L0',
];

// What a straight line of each weight looks like where it meets the cell's edge.
const HORIZONTAL_LINE: Readonly<Record<string, string>> = { L: '─', H: '━', D: '═' };
const VERTICAL_LINE: Readonly<Record<string, string>> = { L: '│', H: '┃', D: '║' };

function reach(arm: string, line: Readonly<Record<string, string>>, edge: Edge): string {
  const straight = line[arm];
  return straight === undefined ? NOTHING : edgeOf(straight, edge);
}

function count(edge: string): number {
  return edge.split('#').length - 1;
}

describe('lines meet across cells', () => {
  it('lists every straight box drawing character', () => {
    expect(UNICODE_ARMS).toHaveLength(109);
  });

  it.each(UNICODE_ARMS)('draws %s with each arm reaching the edge as a straight line', (entry) => {
    const [char = '', arms = ''] = entry.split(' ');
    expect(edgeOf(char, 'top')).toBe(reach(arms.charAt(0), VERTICAL_LINE, 'top'));
    expect(edgeOf(char, 'right')).toBe(reach(arms.charAt(1), HORIZONTAL_LINE, 'right'));
    expect(edgeOf(char, 'bottom')).toBe(reach(arms.charAt(2), VERTICAL_LINE, 'bottom'));
    expect(edgeOf(char, 'left')).toBe(reach(arms.charAt(3), HORIZONTAL_LINE, 'left'));
  });

  it('draws a heavy line twice as thick as a light one and a double line as two strokes', () => {
    expect(count(edgeOf('─', 'left'))).toBe(1);
    expect(count(edgeOf('━', 'left'))).toBe(2);
    expect(count(edgeOf('═', 'left'))).toBe(2);
    expect(edgeOf('━', 'left')).toContain('##');
    expect(edgeOf('═', 'left')).not.toContain('##');
  });
});

// How the arms join at the centre of a 9-pixel cell, where a double or heavy
// line meets another line: the strokes stop at the line they meet and a heavy
// corner is filled square.
const PICTURES: [string, string][] = [
  [
    '┼',
    `
.....#...
.....#...
.....#...
.....#...
.....#...
#########
.....#...
.....#...
.....#...
`,
  ],
  [
    '╬',
    `
....#.#..
....#.#..
....#.#..
....#.#..
#####.###
.........
#####.###
....#.#..
....#.#..
`,
  ],
  [
    '╔',
    `
.........
.........
.........
.........
....#####
....#....
....#.###
....#.#..
....#.#..
`,
  ],
  [
    '╗',
    `
.........
.........
.........
.........
#######..
......#..
#####.#..
....#.#..
....#.#..
`,
  ],
  [
    '╚',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.###
....#....
....#####
.........
.........
`,
  ],
  [
    '╝',
    `
....#.#..
....#.#..
....#.#..
....#.#..
#####.#..
......#..
#######..
.........
.........
`,
  ],
  [
    '╠',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.###
....#....
....#.###
....#.#..
....#.#..
`,
  ],
  [
    '╣',
    `
....#.#..
....#.#..
....#.#..
....#.#..
#####.#..
......#..
#####.#..
....#.#..
....#.#..
`,
  ],
  [
    '╦',
    `
.........
.........
.........
.........
#########
.........
#####.###
....#.#..
....#.#..
`,
  ],
  [
    '╩',
    `
....#.#..
....#.#..
....#.#..
....#.#..
#####.###
.........
#########
.........
.........
`,
  ],
  [
    '╪',
    `
.....#...
.....#...
.....#...
.....#...
#########
.....#...
#########
.....#...
.....#...
`,
  ],
  [
    '╫',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.#..
#########
....#.#..
....#.#..
....#.#..
`,
  ],
  [
    '╒',
    `
.........
.........
.........
.........
.....####
.....#...
.....####
.....#...
.....#...
`,
  ],
  [
    '╓',
    `
.........
.........
.........
.........
.........
....#####
....#.#..
....#.#..
....#.#..
`,
  ],
  [
    '╕',
    `
.........
.........
.........
.........
######...
.....#...
######...
.....#...
.....#...
`,
  ],
  [
    '╖',
    `
.........
.........
.........
.........
.........
#######..
....#.#..
....#.#..
....#.#..
`,
  ],
  [
    '╘',
    `
.....#...
.....#...
.....#...
.....#...
.....####
.....#...
.....####
.........
.........
`,
  ],
  [
    '╙',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.#..
....#####
.........
.........
.........
`,
  ],
  [
    '╛',
    `
.....#...
.....#...
.....#...
.....#...
######...
.....#...
######...
.........
.........
`,
  ],
  [
    '╜',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.#..
#######..
.........
.........
.........
`,
  ],
  [
    '╞',
    `
.....#...
.....#...
.....#...
.....#...
.....####
.....#...
.....####
.....#...
.....#...
`,
  ],
  [
    '╟',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.#..
....#.###
....#.#..
....#.#..
....#.#..
`,
  ],
  [
    '╡',
    `
.....#...
.....#...
.....#...
.....#...
######...
.....#...
######...
.....#...
.....#...
`,
  ],
  [
    '╢',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.#..
#####.#..
....#.#..
....#.#..
....#.#..
`,
  ],
  [
    '╤',
    `
.........
.........
.........
.........
#########
.........
#########
.....#...
.....#...
`,
  ],
  [
    '╥',
    `
.........
.........
.........
.........
.........
#########
....#.#..
....#.#..
....#.#..
`,
  ],
  [
    '╧',
    `
.....#...
.....#...
.....#...
.....#...
#########
.........
#########
.........
.........
`,
  ],
  [
    '╨',
    `
....#.#..
....#.#..
....#.#..
....#.#..
....#.#..
#########
.........
.........
.........
`,
  ],
  [
    '┏',
    `
.........
.........
.........
.........
....#####
....#####
....##...
....##...
....##...
`,
  ],
  [
    '╋',
    `
....##...
....##...
....##...
....##...
#########
#########
....##...
....##...
....##...
`,
  ],
  [
    '┝',
    `
.....#...
.....#...
.....#...
.....#...
....#####
....#####
.....#...
.....#...
.....#...
`,
  ],
  [
    '┓',
    `
.........
.........
.........
.........
######...
######...
....##...
....##...
....##...
`,
  ],
  [
    '┗',
    `
....##...
....##...
....##...
....##...
....#####
....#####
.........
.........
.........
`,
  ],
  [
    '┛',
    `
....##...
....##...
....##...
....##...
######...
######...
.........
.........
.........
`,
  ],
  [
    '┽',
    `
.....#...
.....#...
.....#...
.....#...
######...
#########
.....#...
.....#...
.....#...
`,
  ],
  [
    '╁',
    `
.....#...
.....#...
.....#...
.....#...
....##...
#########
....##...
....##...
....##...
`,
  ],
  [
    '┡',
    `
....##...
....##...
....##...
....##...
....#####
....#####
.....#...
.....#...
.....#...
`,
  ],
  [
    '╃',
    `
....##...
....##...
....##...
....##...
######...
#########
.....#...
.....#...
.....#...
`,
  ],
];

describe('where lines join inside the cell', () => {
  it.each(PICTURES)('draws %s as the junction it is named for', (char, picture) => {
    expect(pictureOf(char, 9)).toBe(picture);
  });
});

describe('block elements fill the part of the cell they are named for', () => {
  const LOWER_EIGHTHS = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
  const LEFT_EIGHTHS = ['█', '▉', '▊', '▋', '▌', '▍', '▎', '▏'];

  it.each(LOWER_EIGHTHS.map((char, index) => [char, index + 1] as const))(
    'fills the bottom of the cell with %s by %i eighths',
    (char, eighths) => {
      const rows = pixelsOf(char, 16).map((row) => row.map((on) => (on ? '#' : '.')).join(''));
      const filled = rows.filter((row) => row === '#'.repeat(16));
      expect(filled).toHaveLength(eighths * 2);
      expect(rows.slice(16 - eighths * 2).every((row) => row === '#'.repeat(16))).toBe(true);
      expect(rows.slice(0, 16 - eighths * 2).every((row) => row === '.'.repeat(16))).toBe(true);
    }
  );

  it.each(LEFT_EIGHTHS.map((char, index) => [char, 8 - index] as const))(
    'fills the left of the cell with %s by %i eighths',
    (char, eighths) => {
      for (const row of pixelsOf(char, 16)) {
        expect(row.map((on) => (on ? '#' : '.')).join('')).toBe(
          '#'.repeat(eighths * 2) + '.'.repeat(16 - eighths * 2)
        );
      }
    }
  );

  it('fills the upper half for ▀ and the right half for ▐', () => {
    expect(pictureOf('▀', 4)).toBe('\n####\n####\n....\n....\n');
    expect(pictureOf('▐', 4)).toBe('\n..##\n..##\n..##\n..##\n');
  });

  it('fills the upper and the right eighth for ▔ and ▕', () => {
    expect(pictureOf('▔', 8)).toBe(`\n########\n${'........\n'.repeat(7)}`);
    expect(pictureOf('▕', 8)).toBe(`\n${'.......#\n'.repeat(8)}`);
  });

  it('fills the quadrants ▖ ▗ ▘ ▙ ▚ ▛ ▜ ▝ ▞ ▟ are named for', () => {
    // Upper left, upper right, lower left, lower right.
    const named: [string, string][] = [
      ['▖', '0010'],
      ['▗', '0001'],
      ['▘', '1000'],
      ['▙', '1011'],
      ['▚', '1001'],
      ['▛', '1110'],
      ['▜', '1101'],
      ['▝', '0100'],
      ['▞', '0110'],
      ['▟', '0111'],
    ];
    for (const [char, quadrants] of named) {
      const grid = pixelsOf(char, 4);
      const probe = [grid[0]?.[0], grid[0]?.[3], grid[3]?.[0], grid[3]?.[3]];
      expect(probe.map((on) => (on === true ? '1' : '0')).join(''), char).toBe(quadrants);
      const filled = grid.flat().filter(Boolean).length;
      expect(filled, char).toBe([...quadrants].filter((q) => q === '1').length * 4);
    }
  });

  it('shades the whole cell at a quarter, a half and three quarters', () => {
    for (const [char, density] of [
      ['░', 0.25],
      ['▒', 0.5],
      ['▓', 0.75],
    ] as const) {
      expect(opsOf(char, 6)).toEqual([
        { kind: 'fill', rect: { x: 0, y: 0, w: 6, h: 6 }, alpha: density },
      ]);
    }
  });
});

describe('dashed lines', () => {
  // Character, axis, dash count and thickness, as the Unicode names give them.
  const DASHED: [string, 'horizontal' | 'vertical', number, 1 | 2][] = [
    ['┄', 'horizontal', 3, 1],
    ['┅', 'horizontal', 3, 2],
    ['┆', 'vertical', 3, 1],
    ['┇', 'vertical', 3, 2],
    ['┈', 'horizontal', 4, 1],
    ['┉', 'horizontal', 4, 2],
    ['┊', 'vertical', 4, 1],
    ['┋', 'vertical', 4, 2],
    ['╌', 'horizontal', 2, 1],
    ['╍', 'horizontal', 2, 2],
    ['╎', 'vertical', 2, 1],
    ['╏', 'vertical', 2, 2],
  ];

  it.each(DASHED)('draws %s as evenly spaced dashes', (char, axis, dashes, thickness) => {
    const size = 48;
    const grid = pixelsOf(char, size);
    const middle = size / 2;
    const line =
      axis === 'horizontal' ? (grid[middle] ?? []) : grid.map((row) => row[middle] === true);
    const text = line.map((on) => (on ? '#' : '.')).join('');
    const runs = [...text.matchAll(/#+/g)].map((run) => [run.index, run.index + run[0].length]);
    expect(runs).toHaveLength(dashes);
    const starts = runs.map(([start = 0]) => start);
    const lengths = runs.map(([start = 0, end = 0]) => end - start);
    // Evenly spaced and equally long; the first starts inside the cell so that cells joined end to end keep the rhythm.
    expect(new Set(starts.slice(1).map((start, i) => start - (starts[i] ?? 0)))).toEqual(
      new Set([size / dashes])
    );
    expect(new Set(lengths).size).toBe(1);
    expect(starts[0]).toBeGreaterThan(0);
    // The strokes are as thick as the weight says.
    const across =
      axis === 'horizontal' ? grid.map((row) => row[starts[0] ?? 0]) : (grid[starts[0] ?? 0] ?? []);
    expect(across.filter(Boolean)).toHaveLength(thickness);
  });
});

describe('diagonals and arcs', () => {
  it('draws ╱ from the top right to the bottom left corner and ╲ the other way', () => {
    expect(opsOf('╱', 8)).toEqual([{ kind: 'stroke', lineWidth: 1, from: [8, 0], to: [0, 8] }]);
    expect(opsOf('╲', 8)).toEqual([{ kind: 'stroke', lineWidth: 1, from: [0, 0], to: [8, 8] }]);
    expect(opsOf('╳', 8)).toHaveLength(2);
  });

  it('draws each arc between the edge midpoints it is named for, bending through the centre', () => {
    const named: [string, [number, number], [number, number]][] = [
      ['╭', [8, 4], [4, 8]],
      ['╮', [0, 4], [4, 8]],
      ['╯', [0, 4], [4, 0]],
      ['╰', [8, 4], [4, 0]],
    ];
    for (const [char, from, to] of named) {
      expect(opsOf(char, 8), char).toEqual([
        { kind: 'curve', lineWidth: 1, from, control: [4, 4], to },
      ]);
    }
  });
});

describe('the stroke follows the device pixel ratio', () => {
  it('draws a heavy line twice as thick as a light one, centred in the cell', () => {
    expect(opsOf('━', 16)).toEqual([
      { kind: 'fill', rect: { x: 0, y: 7, w: 16, h: 2 }, alpha: null },
    ]);
    expect(opsOf('─', 16)).toEqual([
      { kind: 'fill', rect: { x: 0, y: 8, w: 16, h: 1 }, alpha: null },
    ]);
  });

  it('spaces the strokes of a double line by the light width', () => {
    expect(shapeOps(boxGlyphShapes('═') ?? [], 32, 32, 2)).toEqual([
      { kind: 'fill', rect: { x: 0, y: 13, w: 32, h: 2 }, alpha: null },
      { kind: 'fill', rect: { x: 0, y: 17, w: 32, h: 2 }, alpha: null },
    ]);
  });
});

describe('the range drawn without the font', () => {
  it('draws every character from U+2500 to U+259F itself', () => {
    const missing: string[] = [];
    for (let code = 0x2500; code <= 0x259f; code += 1) {
      const shapes = boxGlyphShapes(String.fromCodePoint(code));
      if (shapes === null || shapes.length === 0) missing.push(code.toString(16));
    }
    expect(missing).toEqual([]);
  });

  it('gives every shape a step that draws it', () => {
    for (let code = 0x2500; code <= 0x259f; code += 1) {
      const char = String.fromCodePoint(code);
      const shapes = boxGlyphShapes(char) ?? [];
      expect(shapeOps(shapes, 16, 16, 1), char).toHaveLength(shapes.length);
    }
  });

  it('leaves characters outside the range to the font', () => {
    for (const text of ['a', ' ', '全', '⓿', '■', '', '─́', '──']) {
      expect(boxGlyphShapes(text), JSON.stringify(text)).toBeNull();
    }
  });
});
