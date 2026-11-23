/**
 * The type of the UI's own text. Only a type lives here, so that a component
 * can name it without reading `i18n/`; nothing here makes a value of it.
 */
declare const uiTextBrand: unique symbol;

/**
 * Text the UI itself wrote, resolved from the locale resources. Only
 * `resolveMessages` (`i18n/messages.ts`) makes one.
 */
export type UiText = string & { readonly [uiTextBrand]: true };
