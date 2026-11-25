/**
 * Keeps the main window from shrinking past what the shell can lay out: the
 * rail, the sidebar at its widest, the divider and one window at its minimum
 * side by side, and the rail's own buttons top to bottom.
 */

import { MIN_WINDOW_SIZE, type Size } from '../layout/float-layout';
import { SIDEBAR_WIDTH_MAX } from '../layout/sidebar-width';
import { invokeSetMainWindowMinSize } from '../ipc/session-client';

/** What the shell measures on screen, in CSS pixels. */
export interface ShellExtents {
  /** Right edge of the rail. */
  railRight: number;
  /** Width of the divider between the sidebar and the windows. */
  dividerWidth: number;
  /** Bottom edge the rail's contents need, with no room to spare. */
  railContentBottom: number;
}

/**
 * The smallest main window, in CSS pixels, that shows the whole shell. The
 * sidebar counts at {@link SIDEBAR_WIDTH_MAX}, so widening it never pushes a
 * window off screen.
 */
export function appMinSize(extents: ShellExtents): Size {
  return {
    width: Math.ceil(
      extents.railRight + SIDEBAR_WIDTH_MAX + extents.dividerWidth + MIN_WINDOW_SIZE.width
    ),
    height: Math.ceil(extents.railContentBottom),
  };
}

/** Height `element` takes when it is given only what its contents need. */
function contentHeight(element: HTMLElement): number {
  const { height, alignSelf } = element.style;
  try {
    element.style.height = 'min-content';
    element.style.alignSelf = 'flex-start';
    return element.getBoundingClientRect().height;
  } finally {
    element.style.height = height;
    element.style.alignSelf = alignSelf;
  }
}

/**
 * Svelte action for the shell's layout row: gives the main window the minimum
 * size the rail and the divider inside `node` call for, and again whenever
 * either changes size. A failure is logged and leaves the last minimum in place.
 */
export function fitAppMinSize(node: HTMLElement): { destroy: () => void } {
  const rail = node.querySelector<HTMLElement>('.app-rail');
  const divider = node.querySelector<HTMLElement>('.sidebar-resize');
  if (rail === null || divider === null) {
    console.warn('cannot size the main window: the rail or the divider is not drawn');
    return { destroy: () => {} };
  }
  let applied: Size | null = null;
  const apply = (): void => {
    const railBox = rail.getBoundingClientRect();
    const size = appMinSize({
      railRight: railBox.right,
      dividerWidth: divider.getBoundingClientRect().width,
      railContentBottom: railBox.top + contentHeight(rail),
    });
    if (applied !== null && applied.width === size.width && applied.height === size.height) {
      return;
    }
    applied = size;
    invokeSetMainWindowMinSize(size.width, size.height).catch((error: unknown) => {
      console.warn('failed to set the main window minimum size:', error);
    });
  };
  // The rail stretches with the window, so this also runs on every height change;
  // only a changed minimum crosses to the core.
  const observer = new ResizeObserver(apply);
  observer.observe(rail);
  observer.observe(divider);
  apply();
  return { destroy: () => observer.disconnect() };
}
