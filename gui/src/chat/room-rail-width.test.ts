import { describe, it, expect } from 'vitest';

import {
  ROOM_RAIL_WIDTH_DEFAULT,
  clampRoomRailWidth,
  readStoredRoomRailWidth,
  createRoomRailWidthStorage,
} from './room-rail-width';

const STORAGE_KEY = 'yagura.chat-room-rail-width';

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

describe('room rail width constants', () => {});

describe('clampRoomRailWidth - clamps to [MIN, floor(panelWidth / 2)], boundaries included', () => {
  it.each([
    // [px, panelWidth, expected]
    [119, 1000, 120],
    [0, 1000, 120],
    [-500, 1000, 120],
    [120, 1000, 120],
    [200, 1000, 200],
    [500, 1000, 500],
    [501, 1000, 500],
    [10000, 1000, 500],
    // odd and fractional panel widths: the maximum is floored
    [300, 601, 300],
    [301, 601, 300],
    [301, 600.9, 300],
    [300.5, 601, 300],
  ])('clampRoomRailWidth(%j, %j) is %j', (px, panelWidth, expected) => {
    expect(clampRoomRailWidth(px, panelWidth)).toBe(expected);
  });

  it('the maximum follows the panel width rather than a fixed number', () => {
    expect(clampRoomRailWidth(10000, 800)).toBe(400);
    expect(clampRoomRailWidth(10000, 1400)).toBe(700);
  });
});

describe('clampRoomRailWidth - a narrow panel gives the half-panel maximum priority over the minimum', () => {
  it.each([
    [50, 200, 100],
    [120, 200, 100],
    [150, 200, 100],
    [10000, 200, 100],
    [10, 239, 119],
  ])('clampRoomRailWidth(%j, %j) is %j', (px, panelWidth, expected) => {
    expect(clampRoomRailWidth(px, panelWidth)).toBe(expected);
  });

  it('when half the panel equals the minimum, every width becomes the minimum', () => {
    for (const px of [0, 119, 120, 121, 1000]) {
      expect(clampRoomRailWidth(px, 240)).toBe(120);
    }
  });
});

describe('clampRoomRailWidth - a non-finite width is the default passed through the same range', () => {
  it.each([[NaN], [Infinity], [-Infinity]])('%j on a wide panel gives the default', (px) => {
    expect(clampRoomRailWidth(px, 1000)).toBe(ROOM_RAIL_WIDTH_DEFAULT);
  });

  it.each([[NaN], [Infinity], [-Infinity]])(
    '%j on a panel narrower than twice the default gives half the panel',
    (px) => {
      expect(clampRoomRailWidth(px, 280)).toBe(140);
    }
  );

  it.each([[NaN], [Infinity], [-Infinity]])(
    '%j on a panel narrower than twice the minimum still gives half the panel',
    (px) => {
      expect(clampRoomRailWidth(px, 200)).toBe(100);
    }
  );
});

describe('readStoredRoomRailWidth - unusable values give the default', () => {
  it.each([[null], [''], ['   '], ['not-a-number'], ['12px'], ['Infinity'], ['-Infinity']])(
    '%j gives the default',
    (raw) => {
      let result: number | undefined;
      expect(() => {
        result = readStoredRoomRailWidth(raw);
      }).not.toThrow();
      expect(result).toBe(ROOM_RAIL_WIDTH_DEFAULT);
    }
  );
});

describe('readStoredRoomRailWidth - fixes only values below the minimum, with no upper bound', () => {
  it.each([
    ['119', 120],
    ['0', 120],
    ['-40', 120],
    ['120', 120],
    ['200', 200],
    ['150.5', 150.5],
    // the upper bound depends on the panel, which is applied when drawing
    ['900', 900],
    ['10000', 10000],
  ])('%j reads back as %j', (raw, expected) => {
    expect(readStoredRoomRailWidth(raw)).toBe(expected);
  });
});

describe('createRoomRailWidthStorage - round trip under its own key', () => {
  it(`save() writes under ${STORAGE_KEY}`, () => {
    const { storage, store } = makeFakeStorage();
    createRoomRailWidthStorage(storage).save(300);

    expect([...store.keys()]).toEqual([STORAGE_KEY]);
    expect(Number(store.get(STORAGE_KEY))).toBe(300);
  });

  it(`load() reads what is stored under ${STORAGE_KEY}`, () => {
    const { storage, store } = makeFakeStorage();
    store.set(STORAGE_KEY, '240');
    store.set('yagura.sidebar-width', '400');

    expect(createRoomRailWidthStorage(storage).load()).toBe(240);
  });

  it.each([
    [300, 300],
    [5000, 5000],
    [50, 120],
    [NaN, 160],
  ])('save(%j) then load() gives %j', (saved, expected) => {
    const { storage } = makeFakeStorage();
    const railStorage = createRoomRailWidthStorage(storage);

    railStorage.save(saved);

    expect(railStorage.load()).toBe(expected);
  });

  it('load() gives the default when nothing has been stored yet', () => {
    const { storage } = makeFakeStorage();
    expect(createRoomRailWidthStorage(storage).load()).toBe(ROOM_RAIL_WIDTH_DEFAULT);
  });
});

describe('createRoomRailWidthStorage - degrades instead of throwing when storage is broken', () => {
  it('load() gives the default when getItem throws', () => {
    const throwingStorage: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => {
        throw new Error('boom');
      },
      setItem: () => {},
    };
    let loaded: number | undefined;
    expect(() => {
      loaded = createRoomRailWidthStorage(throwingStorage).load();
    }).not.toThrow();
    expect(loaded).toBe(ROOM_RAIL_WIDTH_DEFAULT);
  });

  it('save() does not throw when setItem throws', () => {
    const throwingStorage: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => null,
      setItem: () => {
        throw new Error('boom');
      },
    };
    expect(() => createRoomRailWidthStorage(throwingStorage).save(300)).not.toThrow();
  });
});
