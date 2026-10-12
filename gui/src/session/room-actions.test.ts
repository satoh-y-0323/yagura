/**
 * The single execution path for the per-room operations: collapse toggling,
 * tiling a room's windows, and minimizing/restoring a room as a whole.
 */
import { describe, it, expect, vi, type Mock } from 'vitest';

import { createRoomActions, type RoomActionsDeps } from './room-actions';

type MockRegistry = {
  [K in keyof RoomActionsDeps['registry']]: Mock<RoomActionsDeps['registry'][K]>;
};
type MockCollapsed = {
  [K in keyof RoomActionsDeps['collapsed']]: Mock<RoomActionsDeps['collapsed'][K]>;
};

function makeDeps(overrides: Partial<RoomActionsDeps> = {}): RoomActionsDeps {
  const registry: MockRegistry = {
    arrangeWindowsOf: vi.fn<RoomActionsDeps['registry']['arrangeWindowsOf']>(),
    setWindowsMinimized: vi.fn<RoomActionsDeps['registry']['setWindowsMinimized']>(),
  };
  const collapsed: MockCollapsed = {
    toggle: vi.fn<RoomActionsDeps['collapsed']['toggle']>(),
  };
  return {
    registry,
    collapsed,
    windowIdsOf: vi.fn<RoomActionsDeps['windowIdsOf']>().mockReturnValue(['w-a', 'w-b']),
    measureArea: vi
      .fn<RoomActionsDeps['measureArea']>()
      .mockReturnValue({ width: 100, height: 50 }),
    ...overrides,
  };
}

describe('createRoomActions', () => {
  it('toggleCollapsed calls deps.collapsed.toggle with the room id', () => {
    const deps = makeDeps();
    const actions = createRoomActions(deps);

    actions.toggleCollapsed('room-a');

    expect(deps.collapsed.toggle).toHaveBeenCalledTimes(1);
    expect(deps.collapsed.toggle).toHaveBeenCalledWith('room-a');
  });

  it('tile arranges the room windows in the measured area', () => {
    const deps = makeDeps({
      windowIdsOf: vi.fn<RoomActionsDeps['windowIdsOf']>().mockReturnValue(['w-a', 'w-b']),
      measureArea: vi
        .fn<RoomActionsDeps['measureArea']>()
        .mockReturnValue({ width: 100, height: 50 }),
    });
    const actions = createRoomActions(deps);

    actions.tile('room-a');

    expect(deps.windowIdsOf).toHaveBeenCalledWith('room-a');
    expect(deps.registry.arrangeWindowsOf).toHaveBeenCalledTimes(1);
    expect(deps.registry.arrangeWindowsOf).toHaveBeenCalledWith({ width: 100, height: 50 }, [
      'w-a',
      'w-b',
    ]);
  });

  it('tile does nothing when the room has no windows', () => {
    const deps = makeDeps({
      windowIdsOf: vi.fn<RoomActionsDeps['windowIdsOf']>().mockReturnValue([]),
    });
    const actions = createRoomActions(deps);

    actions.tile('room-a');

    expect(deps.registry.arrangeWindowsOf).not.toHaveBeenCalled();
  });

  it('tile does nothing when the area cannot be measured', () => {
    const deps = makeDeps({
      measureArea: vi.fn<RoomActionsDeps['measureArea']>().mockReturnValue(null),
    });
    const actions = createRoomActions(deps);

    actions.tile('room-a');

    expect(deps.registry.arrangeWindowsOf).not.toHaveBeenCalled();
  });

  it('minimizeAll minimizes the room windows in one call', () => {
    const deps = makeDeps({
      windowIdsOf: vi.fn<RoomActionsDeps['windowIdsOf']>().mockReturnValue(['w-a', 'w-b']),
    });
    const actions = createRoomActions(deps);

    actions.minimizeAll('room-a');

    expect(deps.registry.setWindowsMinimized).toHaveBeenCalledTimes(1);
    expect(deps.registry.setWindowsMinimized).toHaveBeenCalledWith(['w-a', 'w-b'], true);
  });

  it('restoreAll restores the room windows in one call', () => {
    const deps = makeDeps({
      windowIdsOf: vi.fn<RoomActionsDeps['windowIdsOf']>().mockReturnValue(['w-a', 'w-b']),
    });
    const actions = createRoomActions(deps);

    actions.restoreAll('room-a');

    expect(deps.registry.setWindowsMinimized).toHaveBeenCalledTimes(1);
    expect(deps.registry.setWindowsMinimized).toHaveBeenCalledWith(['w-a', 'w-b'], false);
  });
});
