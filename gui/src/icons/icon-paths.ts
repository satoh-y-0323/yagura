/**
 * Icon shapes copied from Lucide (https://lucide.dev), 24x24 grid, drawn with a 2px stroke.
 *
 * ISC License
 *
 * Copyright (c) 2026 Lucide Icons and Contributors
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
 * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
 * ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
 * OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 *
 * Some of the shapes below derive from the Feather project and carry its notice:
 *
 * The MIT License (MIT) (for the icons listed above)
 *
 * Copyright (c) 2013-present Cole Bemis
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

export type IconShape =
  | { tag: 'path'; attrs: { d: string } }
  | { tag: 'circle'; attrs: { cx: string; cy: string; r: string } }
  | {
      tag: 'rect';
      attrs: { x: string; y: string; width: string; height: string; rx?: string; ry?: string };
    }
  | { tag: 'line'; attrs: { x1: string; y1: string; x2: string; y2: string } };

export const ICON_SHAPES = {
  'arrow-left': [
    { tag: 'path', attrs: { d: 'm12 19-7-7 7-7' } },
    { tag: 'path', attrs: { d: 'M19 12H5' } },
  ],
  'shield-exclamation-point': [
    {
      tag: 'path',
      attrs: {
        d: 'M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z',
      },
    },
    { tag: 'path', attrs: { d: 'M12 8v4' } },
    { tag: 'path', attrs: { d: 'M12 16h.01' } },
  ],
  'message-circle': [
    {
      tag: 'path',
      attrs: {
        d: 'M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092 10 10 0 1 0-4.777-4.719',
      },
    },
  ],
  'circle-dashed': [
    { tag: 'path', attrs: { d: 'M10.1 2.182a10 10 0 0 1 3.8 0' } },
    { tag: 'path', attrs: { d: 'M13.9 21.818a10 10 0 0 1-3.8 0' } },
    { tag: 'path', attrs: { d: 'M17.609 3.721a10 10 0 0 1 2.69 2.7' } },
    { tag: 'path', attrs: { d: 'M2.182 13.9a10 10 0 0 1 0-3.8' } },
    { tag: 'path', attrs: { d: 'M20.279 17.609a10 10 0 0 1-2.7 2.69' } },
    { tag: 'path', attrs: { d: 'M21.818 10.1a10 10 0 0 1 0 3.8' } },
    { tag: 'path', attrs: { d: 'M3.721 6.391a10 10 0 0 1 2.7-2.69' } },
    { tag: 'path', attrs: { d: 'M6.391 20.279a10 10 0 0 1-2.69-2.7' } },
  ],
  'circle-stop': [
    { tag: 'circle', attrs: { cx: '12', cy: '12', r: '10' } },
    { tag: 'rect', attrs: { x: '9', y: '9', width: '6', height: '6', rx: '1' } },
  ],
  'loader-circle': [{ tag: 'path', attrs: { d: 'M21 12a9 9 0 1 1-6.219-8.56' } }],
  circle: [{ tag: 'circle', attrs: { cx: '12', cy: '12', r: '10' } }],
  'circle-x': [
    { tag: 'circle', attrs: { cx: '12', cy: '12', r: '10' } },
    { tag: 'path', attrs: { d: 'm15 9-6 6' } },
    { tag: 'path', attrs: { d: 'm9 9 6 6' } },
  ],
  plus: [
    { tag: 'path', attrs: { d: 'M5 12h14' } },
    { tag: 'path', attrs: { d: 'M12 5v14' } },
  ],
  x: [
    { tag: 'path', attrs: { d: 'M18 6 6 18' } },
    { tag: 'path', attrs: { d: 'm6 6 12 12' } },
  ],
  square: [{ tag: 'rect', attrs: { x: '3', y: '3', width: '18', height: '18', rx: '2' } }],
  minus: [{ tag: 'path', attrs: { d: 'M5 12h14' } }],
  'maximize-2': [
    { tag: 'path', attrs: { d: 'M15 3h6v6' } },
    { tag: 'path', attrs: { d: 'm21 3-7 7' } },
    { tag: 'path', attrs: { d: 'm3 21 7-7' } },
    { tag: 'path', attrs: { d: 'M9 21H3v-6' } },
  ],
  'minimize-2': [
    { tag: 'path', attrs: { d: 'm14 10 7-7' } },
    { tag: 'path', attrs: { d: 'M20 10h-6V4' } },
    { tag: 'path', attrs: { d: 'm3 21 7-7' } },
    { tag: 'path', attrs: { d: 'M4 14h6v6' } },
  ],
  ellipsis: [
    { tag: 'circle', attrs: { cx: '12', cy: '12', r: '1' } },
    { tag: 'circle', attrs: { cx: '19', cy: '12', r: '1' } },
    { tag: 'circle', attrs: { cx: '5', cy: '12', r: '1' } },
  ],
  'layout-grid': [
    { tag: 'rect', attrs: { x: '3', y: '3', width: '7', height: '7', rx: '1' } },
    { tag: 'rect', attrs: { x: '14', y: '3', width: '7', height: '7', rx: '1' } },
    { tag: 'rect', attrs: { x: '14', y: '14', width: '7', height: '7', rx: '1' } },
    { tag: 'rect', attrs: { x: '3', y: '14', width: '7', height: '7', rx: '1' } },
  ],
  'layout-panel-top': [
    { tag: 'rect', attrs: { x: '3', y: '3', width: '18', height: '7', rx: '1' } },
    { tag: 'rect', attrs: { x: '3', y: '14', width: '7', height: '7', rx: '1' } },
    { tag: 'rect', attrs: { x: '14', y: '14', width: '7', height: '7', rx: '1' } },
  ],
  search: [
    { tag: 'path', attrs: { d: 'm21 21-4.34-4.34' } },
    { tag: 'circle', attrs: { cx: '11', cy: '11', r: '8' } },
  ],
  funnel: [
    {
      tag: 'path',
      attrs: {
        d: 'M10 20a1 1 0 0 0 .553.895l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .517-1.341L21.74 4.67A1 1 0 0 0 21 3H3a1 1 0 0 0-.742 1.67l7.225 7.989A2 2 0 0 1 10 14z',
      },
    },
  ],
  'chevron-down': [{ tag: 'path', attrs: { d: 'm6 9 6 6 6-6' } }],
  'chevron-right': [{ tag: 'path', attrs: { d: 'm9 18 6-6-6-6' } }],
  folder: [
    {
      tag: 'path',
      attrs: {
        d: 'M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z',
      },
    },
  ],
  'messages-square': [
    {
      tag: 'path',
      attrs: {
        d: 'M16 10a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 14.286V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z',
      },
    },
    {
      tag: 'path',
      attrs: {
        d: 'M20 9a2 2 0 0 1 2 2v10.286a.71.71 0 0 1-1.212.502l-2.202-2.202A2 2 0 0 0 17.172 19H10a2 2 0 0 1-2-2v-1',
      },
    },
  ],
  bell: [
    { tag: 'path', attrs: { d: 'M10.268 21a2 2 0 0 0 3.464 0' } },
    {
      tag: 'path',
      attrs: {
        d: 'M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326',
      },
    },
  ],
  settings: [
    {
      tag: 'path',
      attrs: {
        d: 'M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915',
      },
    },
    { tag: 'circle', attrs: { cx: '12', cy: '12', r: '3' } },
  ],
  'square-kanban': [
    { tag: 'rect', attrs: { x: '3', y: '3', width: '18', height: '18', rx: '2' } },
    { tag: 'path', attrs: { d: 'M8 7v7' } },
    { tag: 'path', attrs: { d: 'M12 7v4' } },
    { tag: 'path', attrs: { d: 'M16 7v9' } },
  ],
  mail: [
    { tag: 'path', attrs: { d: 'm22 7-8.991 5.727a2 2 0 0 1-2.009 0L2 7' } },
    { tag: 'rect', attrs: { x: '2', y: '4', width: '20', height: '16', rx: '2' } },
  ],
  plug: [
    { tag: 'path', attrs: { d: 'M12 22v-5' } },
    { tag: 'path', attrs: { d: 'M15 8V2' } },
    {
      tag: 'path',
      attrs: { d: 'M17 8a1 1 0 0 1 1 1v4a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1z' },
    },
    { tag: 'path', attrs: { d: 'M9 8V2' } },
  ],
  'scan-text': [
    { tag: 'path', attrs: { d: 'M3 7V5a2 2 0 0 1 2-2h2' } },
    { tag: 'path', attrs: { d: 'M17 3h2a2 2 0 0 1 2 2v2' } },
    { tag: 'path', attrs: { d: 'M21 17v2a2 2 0 0 1-2 2h-2' } },
    { tag: 'path', attrs: { d: 'M7 21H5a2 2 0 0 1-2-2v-2' } },
    { tag: 'path', attrs: { d: 'M7 8h8' } },
    { tag: 'path', attrs: { d: 'M7 12h10' } },
    { tag: 'path', attrs: { d: 'M7 16h6' } },
  ],
  'triangle-exclamation-point': [
    {
      tag: 'path',
      attrs: { d: 'm21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3' },
    },
    { tag: 'path', attrs: { d: 'M12 9v4' } },
    { tag: 'path', attrs: { d: 'M12 17h.01' } },
  ],
  'circle-exclamation-point': [
    { tag: 'circle', attrs: { cx: '12', cy: '12', r: '10' } },
    { tag: 'line', attrs: { x1: '12', y1: '8', x2: '12', y2: '12' } },
    { tag: 'line', attrs: { x1: '12', y1: '16', x2: '12.01', y2: '16' } },
  ],
  info: [
    { tag: 'circle', attrs: { cx: '12', cy: '12', r: '10' } },
    { tag: 'path', attrs: { d: 'M12 16v-4' } },
    { tag: 'path', attrs: { d: 'M12 8h.01' } },
  ],
  check: [{ tag: 'path', attrs: { d: 'M20 6 9 17l-5-5' } }],
  copy: [
    { tag: 'rect', attrs: { x: '8', y: '8', width: '14', height: '14', rx: '2', ry: '2' } },
    { tag: 'path', attrs: { d: 'M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2' } },
  ],
  ticket: [
    {
      tag: 'path',
      attrs: {
        d: 'M2 9a3 3 0 0 1 0 6v2a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-2a3 3 0 0 1 0-6V7a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2Z',
      },
    },
    { tag: 'path', attrs: { d: 'M13 5v2' } },
    { tag: 'path', attrs: { d: 'M13 17v2' } },
    { tag: 'path', attrs: { d: 'M13 11v2' } },
  ],
} as const satisfies Record<string, readonly IconShape[]>;

/** The icons the app can draw; the union keeps a misspelt name from compiling. */
export type IconName = keyof typeof ICON_SHAPES;
