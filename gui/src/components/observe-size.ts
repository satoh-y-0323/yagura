import type { ActionReturn } from 'svelte/action';

export interface ObservedSize {
  width: number;
  height: number;
}

type ResizeCallback = (size: ObservedSize) => void;

/** The size the element is shown at, and where a native drag of its corner is reported. */
export interface ObserveSizeOptions {
  size: ObservedSize;
  onResize: ResizeCallback;
}

function writeSize(node: HTMLElement, size: ObservedSize): void {
  node.style.width = `${size.width}px`;
  node.style.height = `${size.height}px`;
}

/**
 * Reports the node's border box each time the browser reports a size change,
 * which is the only way to read a native `resize: both` drag back out. It is
 * also the only writer of the node's width and height: the native corner
 * writes them behind the framework's back, and the framework skips a style it
 * believes unchanged, so after each report the size shown is written again.
 */
export function observeSize(
  node: HTMLElement,
  options: ObserveSizeOptions
): ActionReturn<ObserveSizeOptions> {
  let current = options;
  writeSize(node, current.size);

  const observer = new ResizeObserver(() => {
    const rect = node.getBoundingClientRect();
    current.onResize({ width: rect.width, height: rect.height });
    // Queued after the callback, so an update it caused lands first and the
    // latest size is the one written, still before the frame is painted.
    queueMicrotask(() => {
      writeSize(node, current.size);
    });
  });
  observer.observe(node);
  return {
    update(next: ObserveSizeOptions): void {
      current = next;
      writeSize(node, current.size);
    },
    destroy(): void {
      observer.disconnect();
    },
  };
}
