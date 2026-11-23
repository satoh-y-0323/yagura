/**
 * The type of the UI's own text. Only types live here, so that a component
 * can name them without reading `i18n/`; nothing here makes a value of them.
 */
import type { DisplayBlock, DisplayLine } from './untrusted-text';

declare const uiTextBrand: unique symbol;

/**
 * Text the UI itself wrote, resolved from the locale resources. Only
 * `resolveMessages` (`i18n/messages.ts`) makes one.
 */
export type UiText = string & { readonly [uiTextBrand]: true };

/**
 * A locale resource's type with every string it yields typed as `UiText`.
 * Text that is already branded keeps its brand, so outside text a message
 * carries never passes for the UI's own. A function keeps its parameters and
 * has its result mapped; arrays and objects are mapped member by member.
 */
export type UiTextOf<T> = T extends UiText | DisplayLine | DisplayBlock
  ? T
  : T extends string
    ? UiText
    : T extends (...args: infer A) => infer R
      ? (...args: A) => UiTextOf<R>
      : T extends object
        ? { [K in keyof T]: UiTextOf<T[K]> }
        : T;
