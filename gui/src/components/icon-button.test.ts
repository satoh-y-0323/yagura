/**
 * A button that cannot act right now still has to be reachable and still has to
 * say why. `disabledReason` keeps it focusable (`aria-disabled`, not `disabled`)
 * and ties the reason to the button, so the reason is not left to a tooltip
 * that keyboard and pointer users may never see.
 */
import { render } from 'svelte/server';
import { describe, it, expect } from 'vitest';

import { sanitizeForDisplay } from '../display/untrusted-text';
import IconButton from './IconButton.svelte';

const LABEL = 'Interrupt';
const REASON = 'Cannot interrupt: the window state is unknown';

function renderedButtonTag(html: string): string {
  const tag = /<button\b[^>]*>/.exec(html);
  if (tag === null) {
    throw new Error(`no <button> in the rendered output: ${html}`);
  }
  return tag[0];
}

function attributeOf(tag: string, name: string): string | null {
  const match = new RegExp(`(?<![\\w-])${name}="([^"]*)"`).exec(tag);
  return match?.[1] ?? null;
}

/** The visible text of the element with `id`, or null when no such element is rendered. */
function textOfElementWithId(html: string, id: string): string | null {
  const open = new RegExp(`<([a-z][a-z0-9]*)\\b[^>]*\\bid="${id}"[^>]*>`).exec(html);
  if (open === null) {
    return null;
  }
  const rest = html.slice(open.index + open[0].length);
  const close = rest.indexOf(`</${open[1]}>`);
  return (close === -1 ? rest : rest.slice(0, close)).replace(/<[^>]*>/g, '').trim();
}

describe('IconButton with disabledReason', () => {
  const { body } = render(IconButton, {
    props: { label: sanitizeForDisplay(LABEL), disabledReason: sanitizeForDisplay(REASON) },
  });
  const tag = renderedButtonTag(body);

  it('stays focusable: aria-disabled is "true" and the disabled attribute is absent', () => {
    expect(attributeOf(tag, 'aria-disabled')).toBe('true');
    expect(/(?<![\w-])disabled(?![\w-])/.test(tag)).toBe(false);
  });

  it('points aria-describedby at an element that carries the reason text', () => {
    const ids = (attributeOf(tag, 'aria-describedby') ?? '').split(/\s+/).filter((id) => id !== '');

    expect(ids.map((id) => textOfElementWithId(body, id))).toContain(REASON);
  });

  it('keeps the accessible name as the label, without the reason mixed in', () => {
    expect(attributeOf(tag, 'aria-label')).toBe(LABEL);
  });
});
