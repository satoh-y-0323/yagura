/**
 * The shape Tauri's `getCurrentWebview().onDragDropEvent()` delivers:
 * `enter` and `drop` carry the dragged paths, `over` and `leave` carry none.
 */
export type RawDropEvent =
  | { type: 'enter'; paths: string[] }
  | { type: 'over' }
  | { type: 'leave' }
  | { type: 'drop'; paths: string[] };

/**
 * The normalized drop signal the add-window dialog reads. `enter`
 * folds into `{ kind: 'over' }` — the drop-hint UI starts on the first
 * `enter`, same as a plain hover-over.
 */
export type DropSignal =
  { kind: 'over' } | { kind: 'drop'; path: string } | { kind: 'leave' } | null;

/**
 * Normalizes a raw Tauri drag-drop event into the frozen {@link DropSignal}
 * shape. `paths` is only inspected for `enter` and `drop`; an empty `paths`
 * array yields `null`, and when several paths are present only the first is
 * kept.
 */
export function normalizeDrop(event: RawDropEvent): DropSignal {
  switch (event.type) {
    case 'enter':
      return event.paths.length > 0 ? { kind: 'over' } : null;
    case 'over':
      return { kind: 'over' };
    case 'leave':
      return { kind: 'leave' };
    case 'drop': {
      const [path] = event.paths;
      return path !== undefined ? { kind: 'drop', path } : null;
    }
  }
}
