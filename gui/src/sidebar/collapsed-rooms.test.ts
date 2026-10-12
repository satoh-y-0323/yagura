/**
 * The sidebar's collapsed-room store: toggling, pruning and expanding.
 */
import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';

import { createCollapsedRooms } from './collapsed-rooms';

describe('createCollapsedRooms', () => {
  it('starts with an empty set', () => {
    const store = createCollapsedRooms();

    expect(get(store)).toEqual(new Set());
  });

  it('toggle adds a room id that is not yet collapsed', () => {
    const store = createCollapsedRooms();

    store.toggle('room-a');

    expect(get(store)).toEqual(new Set(['room-a']));
  });

  it('toggle removes a room id that is already collapsed', () => {
    const store = createCollapsedRooms();

    store.toggle('room-a');
    store.toggle('room-a');

    expect(get(store)).toEqual(new Set());
  });

  it('toggle only changes the room it is called for', () => {
    const store = createCollapsedRooms();

    store.toggle('room-a');
    store.toggle('room-b');

    expect(get(store)).toEqual(new Set(['room-a', 'room-b']));
  });

  it('prune drops room ids that are no longer live', () => {
    const store = createCollapsedRooms();
    store.toggle('room-a');
    store.toggle('room-b');

    store.prune(new Set(['room-a']));

    expect(get(store)).toEqual(new Set(['room-a']));
  });

  it('two independently created stores do not share state', () => {
    const first = createCollapsedRooms();
    const second = createCollapsedRooms();

    first.toggle('room-a');

    expect(get(second)).toEqual(new Set());
  });
});

describe('createCollapsedRooms.expand (expanding is idempotent, never a toggle)', () => {
  it('expands a room that is collapsed', () => {
    const store = createCollapsedRooms();
    store.toggle('room-a');

    store.expand('room-a');

    expect(get(store)).toEqual(new Set());
  });

  it('leaves a room that is already expanded expanded (an expand written as toggle would collapse it)', () => {
    const store = createCollapsedRooms();

    store.expand('room-a');

    expect(get(store)).toEqual(new Set());
  });

  it('leaves an expanded room expanded however many times it is called', () => {
    const store = createCollapsedRooms();
    store.toggle('room-a');

    store.expand('room-a');
    store.expand('room-a');

    expect(get(store)).toEqual(new Set());
  });

  it('only changes the room it is called for', () => {
    const store = createCollapsedRooms();
    store.toggle('room-a');
    store.toggle('room-b');

    store.expand('room-a');

    expect(get(store)).toEqual(new Set(['room-b']));
  });
});
