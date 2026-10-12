import { describe, it, expect } from 'vitest';

import {
  PANEL_MIN_SIZE,
  PANEL_DEFAULT_SIZE,
  clampPanelSize,
  readStoredPanelSize,
  createPanelSizeStorage,
  isResizedByUser,
  panelMaxSize,
  type PanelSize,
} from './panel-size';

/** A window large enough to hold the default size with room to spare. */
const WIDE: PanelSize = { width: 1920, height: 1080 };

/** A window larger than WIDE, whose upper bound must be larger too. */
const WIDER: PanelSize = { width: 2560, height: 1440 };

/** A window smaller than the default size. */
const NARROW: PanelSize = { width: 800, height: 600 };

/** The largest size `viewport` allows: whatever a far larger request is clamped to. */
function upperBound(viewport: PanelSize): PanelSize {
  return clampPanelSize({ width: 1_000_000, height: 1_000_000 }, viewport);
}

function makeFakeStorage(): {
  storage: Pick<Storage, 'getItem' | 'setItem'>;
  store: Map<string, string>;
} {
  const store = new Map<string, string>();
  return {
    storage: {
      getItem: (key) => store.get(key) ?? null,
      setItem: (key, value) => {
        store.set(key, value);
      },
    },
    store,
  };
}

describe('the declared sizes', () => {
  it('has a positive minimum below the default on both axes', () => {
    expect(PANEL_MIN_SIZE.width).toBeGreaterThan(0);
    expect(PANEL_MIN_SIZE.height).toBeGreaterThan(0);
    expect(PANEL_MIN_SIZE.width).toBeLessThan(PANEL_DEFAULT_SIZE.width);
    expect(PANEL_MIN_SIZE.height).toBeLessThan(PANEL_DEFAULT_SIZE.height);
  });
});

describe('clampPanelSize - the minimum and the window bound the size', () => {
  it.each([
    [PANEL_MIN_SIZE],
    [PANEL_DEFAULT_SIZE],
    [{ width: PANEL_MIN_SIZE.width + 1, height: PANEL_MIN_SIZE.height + 1 }],
    [{ width: 700.5, height: 500.25 }],
  ])('returns %j unchanged when it already fits', (size) => {
    expect(clampPanelSize(size, WIDE)).toEqual(size);
  });

  it.each([
    [{ width: 0, height: 0 }],
    [{ width: -500, height: -500 }],
    [{ width: PANEL_MIN_SIZE.width - 1, height: PANEL_MIN_SIZE.height - 1 }],
  ])('raises %j to the minimum', (size) => {
    expect(clampPanelSize(size, WIDE)).toEqual(PANEL_MIN_SIZE);
  });

  it('raises each axis on its own: a narrow but tall size keeps its height', () => {
    expect(clampPanelSize({ width: 10, height: PANEL_DEFAULT_SIZE.height }, WIDE)).toEqual({
      width: PANEL_MIN_SIZE.width,
      height: PANEL_DEFAULT_SIZE.height,
    });
  });

  it.each([[WIDE], [WIDER], [NARROW]])('never lets the panel exceed the window %j', (viewport) => {
    const upper = upperBound(viewport);
    expect(upper.width).toBeLessThanOrEqual(viewport.width);
    expect(upper.height).toBeLessThanOrEqual(viewport.height);
  });

  it('lets the panel grow past the default in a window with room for it', () => {
    const upper = upperBound(WIDE);
    expect(upper.width).toBeGreaterThan(PANEL_DEFAULT_SIZE.width);
    expect(upper.height).toBeGreaterThan(PANEL_DEFAULT_SIZE.height);
  });

  it('follows the window: a larger window allows a larger panel', () => {
    const wide = upperBound(WIDE);
    const wider = upperBound(WIDER);
    expect(wider.width).toBeGreaterThan(wide.width);
    expect(wider.height).toBeGreaterThan(wide.height);
  });

  it('rounds anything past the bound to the bound itself', () => {
    const upper = upperBound(WIDE);
    expect(clampPanelSize({ width: upper.width + 1, height: upper.height + 1 }, WIDE)).toEqual(
      upper
    );
  });

  it('keeps inside a window smaller than the minimum: the window wins', () => {
    const viewport = { ...PANEL_MIN_SIZE };
    const clamped = clampPanelSize(PANEL_DEFAULT_SIZE, viewport);
    expect(clamped.width).toBeLessThanOrEqual(viewport.width);
    expect(clamped.height).toBeLessThanOrEqual(viewport.height);
    expect(clamped.width).toBeGreaterThan(0);
    expect(clamped.height).toBeGreaterThan(0);
  });

  it.each([[NaN], [Infinity], [-Infinity]])(
    'falls to the default for a non-finite axis (%j) and keeps the other axis',
    (bad) => {
      expect(clampPanelSize({ width: bad, height: 500 }, WIDE)).toEqual({
        width: PANEL_DEFAULT_SIZE.width,
        height: 500,
      });
      expect(clampPanelSize({ width: 700, height: bad }, WIDE)).toEqual({
        width: 700,
        height: PANEL_DEFAULT_SIZE.height,
      });
    }
  );
});

describe('panelMaxSize - the largest size the window allows, the same bound clampPanelSize rounds to', () => {
  // Wide, wider, narrow, between the minimum and the minimum plus the margin, and below the minimum.
  const VIEWPORTS: ReadonlyArray<[PanelSize]> = [
    [WIDE],
    [WIDER],
    [NARROW],
    [{ width: PANEL_MIN_SIZE.width + 40, height: PANEL_MIN_SIZE.height + 40 }],
    [{ ...PANEL_MIN_SIZE }],
    [{ width: 300, height: 200 }],
  ];

  it.each(VIEWPORTS)(
    'equals the bound clampPanelSize rounds a far larger size to, in %j',
    (viewport) => {
      expect(panelMaxSize(viewport)).toEqual(upperBound(viewport));
    }
  );

  it.each(VIEWPORTS)('is never below the minimum the window can show, in %j', (viewport) => {
    const max = panelMaxSize(viewport);
    expect(max.width).toBeGreaterThanOrEqual(Math.min(PANEL_MIN_SIZE.width, viewport.width));
    expect(max.height).toBeGreaterThanOrEqual(Math.min(PANEL_MIN_SIZE.height, viewport.height));
  });

  it.each(VIEWPORTS)('bounds every size clampPanelSize gives, in %j', (viewport) => {
    const max = panelMaxSize(viewport);
    for (const size of [
      PANEL_MIN_SIZE,
      PANEL_DEFAULT_SIZE,
      { width: 0, height: 0 },
      { width: 5000, height: 5000 },
      { width: NaN, height: Infinity },
      { width: viewport.width, height: viewport.height },
    ]) {
      const clamped = clampPanelSize(size, viewport);
      expect(clamped.width, JSON.stringify(size)).toBeLessThanOrEqual(max.width);
      expect(clamped.height, JSON.stringify(size)).toBeLessThanOrEqual(max.height);
    }
  });
});

describe('clampPanelSize - a smaller window only changes the size shown', () => {
  it('gives back the chosen size once the window is wide again', () => {
    const chosen = { width: 1500, height: 900 };
    const whileNarrow = clampPanelSize(chosen, NARROW);
    expect(whileNarrow.width).toBeLessThanOrEqual(NARROW.width);
    expect(whileNarrow.height).toBeLessThanOrEqual(NARROW.height);
    expect(clampPanelSize(chosen, WIDE)).toEqual(chosen);
  });

  it('fits the default size into a window too small for it', () => {
    const shown = clampPanelSize(PANEL_DEFAULT_SIZE, NARROW);
    expect(shown.width).toBeLessThanOrEqual(NARROW.width);
    expect(shown.height).toBeLessThanOrEqual(NARROW.height);
  });
});

describe('readStoredPanelSize - a stored size is the chosen size, read without the window', () => {
  it('reads back a stored size that fits', () => {
    expect(readStoredPanelSize('{"width":700,"height":500}')).toEqual({
      width: 700,
      height: 500,
    });
  });

  it('raises a stored size below the minimum to the minimum', () => {
    expect(readStoredPanelSize('{"width":1,"height":1}')).toEqual(PANEL_MIN_SIZE);
  });

  it('keeps a stored size larger than any window as stored', () => {
    expect(readStoredPanelSize('{"width":100000,"height":100000}')).toEqual({
      width: 100000,
      height: 100000,
    });
  });

  it('falls to the default for a non-finite stored axis', () => {
    expect(readStoredPanelSize('{"width":1e999,"height":1e999}')).toEqual(PANEL_DEFAULT_SIZE);
    const oneAxis = readStoredPanelSize('{"width":1e999,"height":500}');
    expect(oneAxis.width).toBe(PANEL_DEFAULT_SIZE.width);
    expect(Number.isFinite(oneAxis.height)).toBe(true);
  });

  it.each([
    [null],
    [''],
    ['   '],
    ['not json'],
    ['{"width":700,'],
    ['null'],
    ['42'],
    ['"700x500"'],
    ['[700,500]'],
    ['{}'],
    ['{"width":700}'],
    ['{"height":500}'],
    ['{"width":"700","height":"500"}'],
    ['{"width":null,"height":500}'],
    ['{"width":700,"height":true}'],
  ])('falls to the default for a missing or broken value %j', (raw) => {
    let result: PanelSize | undefined;
    expect(() => {
      result = readStoredPanelSize(raw);
    }).not.toThrow();
    expect(result).toEqual(PANEL_DEFAULT_SIZE);
  });
});

describe('createPanelSizeStorage("settings") - the last size survives a reopen', () => {
  it('load() gives back what save() stored', () => {
    const { storage } = makeFakeStorage();
    const sizes = createPanelSizeStorage('settings', storage);

    sizes.save({ width: 700, height: 500 });

    expect(sizes.load()).toEqual({ width: 700, height: 500 });
  });

  it('a second storage over the same backing store reads what the first saved', () => {
    const { storage } = makeFakeStorage();

    createPanelSizeStorage('settings', storage).save({ width: 1100, height: 750 });

    expect(createPanelSizeStorage('settings', storage).load()).toEqual({
      width: 1100,
      height: 750,
    });
  });

  it('a later save replaces the earlier one', () => {
    const { storage } = makeFakeStorage();
    const sizes = createPanelSizeStorage('settings', storage);

    sizes.save({ width: 700, height: 500 });
    sizes.save({ width: 900, height: 600 });

    expect(sizes.load()).toEqual({ width: 900, height: 600 });
  });

  it('gives back a saved size larger than the window as saved, leaving the fit to the size shown', () => {
    const { storage } = makeFakeStorage();
    const sizes = createPanelSizeStorage('settings', storage);

    sizes.save(upperBound(WIDER));

    expect(sizes.load()).toEqual(upperBound(WIDER));
    expect(clampPanelSize(sizes.load(), NARROW)).toEqual(upperBound(NARROW));
  });

  it('gives the default when nothing has been saved yet', () => {
    const { storage } = makeFakeStorage();

    expect(createPanelSizeStorage('settings', storage).load()).toEqual(PANEL_DEFAULT_SIZE);
  });

  it('keeps its value under a key of its own, apart from the sidebar width', () => {
    const { storage, store } = makeFakeStorage();

    createPanelSizeStorage('settings', storage).save({ width: 700, height: 500 });

    expect(store.size).toBe(1);
    expect(store.has('yagura.sidebar-width')).toBe(false);
  });
});

describe('createPanelSizeStorage("settings") - a broken storage degrades to the default', () => {
  it('load() gives the default without throwing when getItem throws', () => {
    const throwing: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => {
        throw new Error('boom');
      },
      setItem: () => {},
    };
    let loaded: PanelSize | undefined;
    expect(() => {
      loaded = createPanelSizeStorage('settings', throwing).load();
    }).not.toThrow();
    expect(loaded).toEqual(PANEL_DEFAULT_SIZE);
  });

  it('save() does not throw when setItem throws', () => {
    const throwing: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => null,
      setItem: () => {
        throw new Error('boom');
      },
    };
    expect(() =>
      createPanelSizeStorage('settings', throwing).save({ width: 700, height: 500 })
    ).not.toThrow();
  });

  it('load() gives the default when the stored value is corrupted', () => {
    const corrupted: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => '{"width":',
      setItem: () => {},
    };
    expect(createPanelSizeStorage('settings', corrupted).load()).toEqual(PANEL_DEFAULT_SIZE);
  });
});

describe('createPanelSizeStorage("plans") - the Plans panel remembers its size too', () => {
  it('load() gives back what save() stored, also through a second storage', () => {
    const { storage } = makeFakeStorage();

    createPanelSizeStorage('plans', storage).save({ width: 1100, height: 750 });

    expect(createPanelSizeStorage('plans', storage).load()).toEqual({
      width: 1100,
      height: 750,
    });
  });

  it('gives the default when nothing has been saved yet', () => {
    const { storage } = makeFakeStorage();

    expect(createPanelSizeStorage('plans', storage).load()).toEqual(PANEL_DEFAULT_SIZE);
  });

  it('gives back a saved size larger than the window as saved, leaving the fit to the size shown', () => {
    const { storage } = makeFakeStorage();
    const sizes = createPanelSizeStorage('plans', storage);

    sizes.save(upperBound(WIDER));

    expect(sizes.load()).toEqual(upperBound(WIDER));
    expect(clampPanelSize(sizes.load(), NARROW)).toEqual(upperBound(NARROW));
  });

  it.each([['{"width":'], ['[700,500]'], ['{"width":"700","height":"500"}'], ['   ']])(
    'gives the default for a broken stored value %j',
    (raw) => {
      const corrupted: Pick<Storage, 'getItem' | 'setItem'> = {
        getItem: () => raw,
        setItem: () => {},
      };
      expect(createPanelSizeStorage('plans', corrupted).load()).toEqual(PANEL_DEFAULT_SIZE);
    }
  );

  it('load() gives the default without throwing when getItem throws, and save() does not throw', () => {
    const throwing: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => {
        throw new Error('boom');
      },
      setItem: () => {
        throw new Error('boom');
      },
    };
    const sizes = createPanelSizeStorage('plans', throwing);
    let loaded: PanelSize | undefined;
    expect(() => {
      loaded = sizes.load();
    }).not.toThrow();
    expect(loaded).toEqual(PANEL_DEFAULT_SIZE);
    expect(() => sizes.save({ width: 700, height: 500 })).not.toThrow();
  });
});

describe('createPanelSizeStorage - the Settings and the Plans panel each keep a size of their own', () => {
  it('a size saved for one panel is not what the other opens at', () => {
    const { storage } = makeFakeStorage();

    createPanelSizeStorage('settings', storage).save({ width: 700, height: 500 });

    expect(createPanelSizeStorage('plans', storage).load()).toEqual(PANEL_DEFAULT_SIZE);
  });

  it('keeps both sizes side by side, each under a key of its own', () => {
    const { storage, store } = makeFakeStorage();

    createPanelSizeStorage('settings', storage).save({ width: 700, height: 500 });
    createPanelSizeStorage('plans', storage).save({ width: 1100, height: 750 });

    expect(store.size).toBe(2);
    expect(createPanelSizeStorage('settings', storage).load()).toEqual({
      width: 700,
      height: 500,
    });
    expect(createPanelSizeStorage('plans', storage).load()).toEqual({
      width: 1100,
      height: 750,
    });
  });

  it('a later save for one panel leaves the other panel as it was', () => {
    const { storage } = makeFakeStorage();
    createPanelSizeStorage('plans', storage).save({ width: 1100, height: 750 });

    createPanelSizeStorage('settings', storage).save({ width: 700, height: 500 });
    createPanelSizeStorage('settings', storage).save({ width: 900, height: 600 });

    expect(createPanelSizeStorage('plans', storage).load()).toEqual({
      width: 1100,
      height: 750,
    });
  });
});

describe('isResizedByUser - a read-back size is the user dragging the corner only when it differs by half a pixel or more', () => {
  const shown: PanelSize = { width: 640, height: 520 };

  it('reads the same size back as no resize', () => {
    expect(isResizedByUser({ ...shown }, shown)).toBe(false);
  });

  it.each<[number, number]>([
    [0.49, 0],
    [0, 0.49],
    [-0.49, 0],
    [0, -0.49],
    [0.3, -0.3],
  ])(
    'treats a difference under half a pixel (%s, %s) as the same size, since the bounding box carries fractions',
    (dx, dy) => {
      expect(isResizedByUser({ width: shown.width + dx, height: shown.height + dy }, shown)).toBe(
        false
      );
    }
  );

  it.each<[number, number]>([
    [0.5, 0],
    [0, 0.5],
    [-0.5, 0],
    [0, -0.5],
    [40, 0],
    [0, -40],
    [12, 12],
  ])('treats a difference of half a pixel or more (%s, %s) as a resize', (dx, dy) => {
    expect(isResizedByUser({ width: shown.width + dx, height: shown.height + dy }, shown)).toBe(
      true
    );
  });
});

describe('panelMaxSize: exact bounds', () => {
  it('leaves a margin of 96px on each axis in a window with room', () => {
    expect(panelMaxSize({ width: 1000, height: 800 })).toEqual({ width: 904, height: 704 });
  });

  it('never goes below what the window can show of the minimum', () => {
    expect(panelMaxSize({ width: 400, height: 300 })).toEqual({ width: 360, height: 280 });
  });

  it('gives the whole window when it is smaller than the minimum', () => {
    expect(panelMaxSize({ width: 200, height: 100 })).toEqual({ width: 200, height: 100 });
  });
});

describe('clampPanelSize: exact values', () => {
  it('keeps a size that fits and brings the others to the nearest bound', () => {
    const viewport = { width: 1000, height: 800 };

    expect(clampPanelSize({ width: 500, height: 400 }, viewport)).toEqual({
      width: 500,
      height: 400,
    });
    expect(clampPanelSize({ width: 100, height: 100 }, viewport)).toEqual({
      width: 360,
      height: 280,
    });
    expect(clampPanelSize({ width: 5000, height: 5000 }, viewport)).toEqual({
      width: 904,
      height: 704,
    });
  });

  it('replaces a non-finite axis by the default of that axis before bounding it', () => {
    const viewport = { width: 1000, height: 800 };

    expect(
      clampPanelSize({ width: Number.NaN, height: Number.POSITIVE_INFINITY }, viewport)
    ).toEqual({
      width: 640,
      height: 520,
    });
  });
});

describe('readStoredPanelSize: what is not trusted', () => {
  const DEFAULT = { width: 640, height: 520 };

  it.each<[string, string | null]>([
    ['nothing stored', null],
    ['an empty text', ''],
    ['a blank text', '   '],
    ['text that is not JSON', '{nope'],
    ['JSON null', 'null'],
    ['a JSON number', '5'],
    ['a JSON array', '[400,300]'],
    ['a missing width', '{"height":300}'],
    ['a missing height', '{"width":400}'],
    ['a string width', '{"width":"400","height":300}'],
    ['a string height', '{"width":400,"height":"300"}'],
  ])('falls to the default for %s', (_label, raw) => {
    expect(readStoredPanelSize(raw)).toEqual(DEFAULT);
  });

  it('returns a copy of the default each time', () => {
    const first = readStoredPanelSize(null);
    first.width = 1;

    expect(readStoredPanelSize(null)).toEqual(DEFAULT);
  });

  it('reads a well-formed size, raising an axis below the minimum', () => {
    expect(readStoredPanelSize('{"width":500,"height":400}')).toEqual({ width: 500, height: 400 });
    expect(readStoredPanelSize('{"width":10,"height":10}')).toEqual({ width: 360, height: 280 });
  });

  it('falls to the default of an axis whose number is not finite', () => {
    expect(readStoredPanelSize('{"width":1e999,"height":400}')).toEqual({
      width: 640,
      height: 400,
    });
    expect(readStoredPanelSize('{"width":500,"height":1e999}')).toEqual({
      width: 500,
      height: 520,
    });
  });
});

describe('isResizedByUser: the tolerance', () => {
  const shown = { width: 640, height: 520 };

  it('reads a difference under half a pixel as no resize, on either axis', () => {
    expect(isResizedByUser({ width: 640.4, height: 520 }, shown)).toBe(false);
    expect(isResizedByUser({ width: 640, height: 519.6 }, shown)).toBe(false);
  });

  it('reads a difference of half a pixel or more as a resize, on either axis and either way', () => {
    expect(isResizedByUser({ width: 640.5, height: 520 }, shown)).toBe(true);
    expect(isResizedByUser({ width: 639.5, height: 520 }, shown)).toBe(true);
    expect(isResizedByUser({ width: 640, height: 520.5 }, shown)).toBe(true);
    expect(isResizedByUser({ width: 640, height: 519.5 }, shown)).toBe(true);
  });
});
