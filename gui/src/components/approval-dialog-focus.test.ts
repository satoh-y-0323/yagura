/**
 * Where the browser puts focus when the approval dialog opens must be the
 * Deny button no matter what else the dialog contains. The rendered HTML is
 * read with the HTML focus delegate steps: a focusable descendant carrying
 * `autofocus` wins over the first focusable element in tree order.
 */
import { render } from 'svelte/server';
import { describe, it, expect } from 'vitest';

import type { ApprovalRequest } from '../ipc/session-boundary';
import { buttons } from '../ui-messages';

import ApprovalDialog from './ApprovalDialog.svelte';

interface RenderedElement {
  tag: string;
  attributes: string;
  text: string;
}

const START_TAG = /<([a-zA-Z][\w-]*)((?:\s+[^\s=>/]+(?:="[^"]*")?)*)\s*\/?>/g;

function hasAttribute(attributes: string, name: string): boolean {
  return new RegExp(`(?:^|\\s)${name}(?=[\\s=]|$)`).test(attributes);
}

function isFocusable({ tag, attributes }: RenderedElement): boolean {
  if (hasAttribute(attributes, 'disabled') || hasAttribute(attributes, 'inert')) {
    return false;
  }
  if (hasAttribute(attributes, 'tabindex') || hasAttribute(attributes, 'contenteditable')) {
    return true;
  }
  if (tag === 'a') {
    return hasAttribute(attributes, 'href');
  }
  return ['button', 'input', 'select', 'textarea'].includes(tag);
}

/** Every start tag of `html` in tree order, with the text that follows it up to the next tag. */
function elementsOf(html: string): RenderedElement[] {
  return [...html.matchAll(START_TAG)].map((match) => {
    const after = html.slice((match.index ?? 0) + match[0].length);
    const next = after.indexOf('<');
    return {
      tag: (match[1] ?? '').toLowerCase(),
      attributes: match[2] ?? '',
      text: (next === -1 ? after : after.slice(0, next)).trim(),
    };
  });
}

function renderApproval(request: ApprovalRequest, pendingCount: number): RenderedElement[] {
  const { body } = render(ApprovalDialog, {
    props: {
      isOpen: true,
      request,
      isRequesterOpen: true,
      pendingCount,
      openedAt: 0,
      onDecide: () => {},
    },
  });
  return elementsOf(body);
}

const WINDOW_REQUEST: ApprovalRequest = {
  request_id: 'r1',
  tool: 'send_message',
  requester: { kind: 'window', window_id: 'w-a', name: 'alpha' },
  target: { window_id: 'w-b', name: 'beta', status: 'idle' },
  text: 'hello',
};

const EXTERNAL_NO_BODY: ApprovalRequest = {
  request_id: 'r2',
  tool: 'open_window',
  requester: { kind: 'external', window_id: null, name: null },
  text: null,
};

const HTML_LOOKING_BODY: ApprovalRequest = {
  ...WINDOW_REQUEST,
  request_id: 'r3',
  text: '<button>Approve now</button> <a href="#">x</a>',
};

describe('ApprovalDialog: where the dialog puts focus on opening', () => {
  it.each([
    ['a window request with a body', WINDOW_REQUEST, 1],
    ['an external request without a body', EXTERNAL_NO_BODY, 3],
    ['a body that looks like markup', HTML_LOOKING_BODY, 2],
  ])('%s: the single autofocus element is the Deny button', (_name, request, pending) => {
    const elements = renderApproval(request, pending);

    const delegates = elements.filter(
      (element) => isFocusable(element) && hasAttribute(element.attributes, 'autofocus')
    );

    expect(
      elements.filter((element) => hasAttribute(element.attributes, 'autofocus'))
    ).toHaveLength(1);
    expect(delegates).toHaveLength(1);
    expect(delegates[0]?.tag).toBe('button');
    expect(delegates[0]?.text).toBe(buttons.deny);
  });
});
