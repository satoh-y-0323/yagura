import { describe, it, expect } from 'vitest';

import appSource from '../App.svelte?raw';
import sessionClientSource from '../ipc/session-client.ts?raw';
import { findTagEnd, functionBody, resolveConditionExpression } from '../test-utils/markup-scan';

/**
 * `ApprovalDialog.svelte` is read through `import.meta.glob`: a direct
 * `?raw` import of a missing file fails the whole file's collection,
 * reporting 0 tests instead of which contracts are unmet.
 *
 * Every contract below is measured by scanning source text, with no jsdom:
 * where focus actually lands, the real stacking order, and the 500 ms guard
 * as observed by a real click are left to the manual check and are
 * absent here.
 */
const approvalDialogFiles = import.meta.glob('./ApprovalDialog.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
});
const source = Object.values(approvalDialogFiles)[0] ?? '';

/** The opening tag beginning at `marker`, including its closing `>`; '' when absent. */
function openingTag(text: string, marker: string): string {
  const start = text.indexOf(marker);
  if (start === -1) {
    return '';
  }
  const end = findTagEnd(text, start);
  return end === -1 ? '' : text.slice(start, end + 1);
}

/**
 * The expression bound to `attribute` in `tag`: `x={expr}` gives `expr`, the
 * shorthand `{x}` gives `x`, '' when the attribute is absent.
 */
function boundExpression(tag: string, attribute: string): string {
  const explicit = tag.match(new RegExp(`${attribute}=\\{([^{}]*)\\}`));
  if (explicit !== null) {
    return explicit[1]?.trim() ?? '';
  }
  return new RegExp(`\\{\\s*${attribute}\\s*\\}`).test(tag) ? attribute : '';
}

/** The opening tag of the `<button>` element whose content or attributes contain `marker`. */
function buttonTagContaining(text: string, marker: string): string {
  const markerIndex = text.indexOf(marker);
  if (markerIndex === -1) {
    return '';
  }
  const start = text.lastIndexOf('<button', markerIndex);
  if (start === -1) {
    return '';
  }
  const end = findTagEnd(text, start);
  return end === -1 ? '' : text.slice(start, end + 1);
}

describe('ApprovalDialog.svelte: non-dismissable', () => {
  it('passes onCancel a no-op handler of its own, not one that calls onApprove/onDeny or closes the dialog', () => {
    const tag = openingTag(source, '<ModalDialog');
    expect(tag).not.toBe('');
    const delegated = boundExpression(tag, 'onCancel');
    expect(delegated).not.toBe('');
    // The shorthand `{onCancel}` would forward the (absent) onCancel prop
    // straight through, giving Escape/backdrop-click a dismissal route.
    expect(delegated).not.toBe('onCancel');

    const body = functionBody(source, delegated);
    expect(body).not.toBe('');
    // A true no-op: it must not itself flip isOpen, and must not reach the
    // Approve/Deny decision path.
    expect(body).not.toMatch(/(?<![\w-])isOpen\s*=/);
    expect(body).not.toContain('decide_approval');
  });

  it('declares no exported onCancel prop (nothing outside owns the dismissal decision)', () => {
    expect(source).not.toMatch(/export let onCancel/);
  });
});

describe('ApprovalDialog.svelte: Deny and Approve buttons', () => {
  it('places Deny before Approve in markup order (the default first-focusable, matching ConfirmDialog)', () => {
    const denyIndex = source.indexOf('buttons.deny');
    const approveIndex = source.indexOf('buttons.approve');
    expect(denyIndex).toBeGreaterThanOrEqual(0);
    expect(approveIndex).toBeGreaterThanOrEqual(0);
    expect(denyIndex).toBeLessThan(approveIndex);
  });

  it('each button calls isWithinGuard as the first statement of its click handler body', () => {
    for (const marker of ['buttons.deny', 'buttons.approve']) {
      const tag = buttonTagContaining(source, marker);
      expect(tag).not.toBe('');
      const handlerName = boundExpression(tag, 'on:click');
      expect(handlerName).not.toBe('');
      const body = functionBody(source, handlerName);
      expect(body).not.toBe('');
      // First statement after the opening brace: isWithinGuard gates entry to
      // the handler, so a click inside the 500 ms guard performs nothing.
      const afterBrace = body.slice(1).trimStart();
      expect(afterBrace.startsWith('if')).toBe(true);
      expect(afterBrace.slice(0, afterBrace.indexOf('{'))).toContain('isWithinGuard(');
    }
  });
});

describe('ApprovalDialog.svelte: decide_approval invocation', () => {
  it('somewhere the dialog (or a module it calls into) invokes decide_approval with request_id and approved', () => {
    // The Tauri call site is not pinned to this one file: App.svelte may own
    // it (as it owns close/interrupt today) or a session-client wrapper may.
    // What is pinned is that the two arguments the daemon requires are both
    // supplied wherever the call is made.
    const haystacks = [source, appSource, sessionClientSource];
    const found = haystacks.some((text) => {
      const callIndex = text.indexOf("'decide_approval'");
      if (callIndex === -1) {
        return false;
      }
      const window = text.slice(callIndex, callIndex + 300);
      return window.includes('request_id') && window.includes('approved');
    });
    expect(found).toBe(true);
  });
});

describe('ApprovalDialog.svelte: body display (full text, sanitized, no remaining-time display)', () => {
  it('passes the body text through a sanitize call before display', () => {
    expect(source).toMatch(/sanitize\w*\(/);
  });
});

describe('App.svelte: renders ApprovalDialog exactly once, gated on the other dialogs being closed', () => {
  /**
   * "Does not open while another modal is open" has to
   * be derived from App's own dialog-open state, not re-declared: the four
   * states that already gate the existing dialogs are isAddDialogOpen,
   * confirmDialog.isOpen, renameDialog.isOpen and isRestoreDialogOpen (all
   * present in App.svelte today, none of them owned by ApprovalDialog).
   */
  it("the isOpen expression passed to <ApprovalDialog> is derived from all four of the app's existing dialog-open states", () => {
    const tag = openingTag(appSource, '<ApprovalDialog');
    expect(tag).not.toBe('');
    const expression = boundExpression(tag, 'isOpen');
    expect(expression).not.toBe('');
    const resolved = resolveConditionExpression(appSource, expression);

    for (const identifier of [
      'isAddDialogOpen',
      'confirmDialog.isOpen',
      'renameDialog.isOpen',
      'isRestoreDialogOpen',
    ]) {
      expect(resolved).toContain(identifier);
    }
  });
});
