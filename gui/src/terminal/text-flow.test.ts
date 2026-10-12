import { describe, it, expect } from 'vitest';
import { TextFlow } from './text-flow';

describe('text typed into the hidden textarea', () => {
  it('sends the text typed and empties the textarea', () => {
    const flow = new TextFlow();
    expect(flow.input('a')).toEqual({ clearsTextarea: true, send: 'a' });
  });

  it('empties the textarea for an input that typed nothing for the window', () => {
    expect(new TextFlow().input(null)).toEqual({ clearsTextarea: true, send: null });
  });

  it('keeps the input method text in the textarea while it composes', () => {
    const flow = new TextFlow();
    flow.startComposition();
    expect(flow.input(null)).toEqual({ clearsTextarea: false, send: null });
  });

  it('empties the textarea again once the composition ended', () => {
    const flow = new TextFlow();
    flow.startComposition();
    flow.endComposition('か');
    expect(flow.input('a').clearsTextarea).toBe(true);
  });
});

describe('a committed composition', () => {
  it('sends the committed text', () => {
    const flow = new TextFlow();
    flow.startComposition();
    expect(flow.endComposition('漢字')).toBe('漢字');
  });

  it('sends nothing when the input method committed nothing', () => {
    expect(new TextFlow().endComposition('')).toBeNull();
  });
});
