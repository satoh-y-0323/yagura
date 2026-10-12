/**
 * What the hidden textarea's input and composition events mean for the
 * window: which text to send, and when the textarea is emptied. The window
 * receives text once it is committed, never while an input method composes it.
 */
export class TextFlow {
  private isComposing = false;

  startComposition(): void {
    this.isComposing = true;
  }

  /**
   * The composition ended with `committed`: the text to send, or `null` when
   * the input method committed nothing.
   */
  endComposition(committed: string): string | null {
    this.isComposing = false;
    return committed === '' ? null : committed;
  }

  /**
   * An input event typed `text` (`null` when it typed none for the window).
   * Outside a composition the textarea is emptied after each input so it never
   * grows; inside one it holds the input method's text.
   */
  input(text: string | null): { clearsTextarea: boolean; send: string | null } {
    return { clearsTextarea: !this.isComposing, send: text };
  }
}
