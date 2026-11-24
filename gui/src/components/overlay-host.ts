/**
 * Where a floating element (tooltip, menu) is drawn. A modal dialog makes the
 * rest of the page inert and paints above it, so anything floating on behalf of
 * an element inside an open modal must live inside that modal; everything else
 * goes to the application's overlay layer.
 */

interface Queryable<E> {
  querySelector: (selector: string) => E | null;
}

/**
 * The host for a floating element opened from `origin`: the open modal's own
 * `.modal-overlay-host` when `origin` is inside one, else the `#overlay-layer`
 * under `root`. Null when neither exists.
 */
export function resolveOverlayHost<E>(
  origin: { closest: (selector: string) => Queryable<E> | null },
  root: Queryable<E>
): E | null {
  const modal = origin.closest('dialog[open]');
  return modal?.querySelector('.modal-overlay-host') ?? root.querySelector('#overlay-layer');
}

/**
 * Svelte action: moves `node` into the host for `origin` and keeps it there.
 * Without a host (the layer is not mounted yet) the node goes to `document.body`.
 */
export function overlayPortal(node: HTMLElement, origin: Element | null) {
  function place(from: Element | null): void {
    const host = from === null ? null : resolveOverlayHost(from, document);
    const target = host ?? document.body;
    if (node.parentElement !== target) {
      target.appendChild(node);
    }
  }

  place(origin);

  return {
    update(next: Element | null): void {
      place(next);
    },
    destroy(): void {
      node.remove();
    },
  };
}
