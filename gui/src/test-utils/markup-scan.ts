/**
 * Finds the index of the `>` that closes a tag, scanning forward from
 * `startIndex` while tracking Svelte `{...}` expression depth and quoted
 * attribute-value state. A naive `indexOf('>', startIndex)` breaks on
 * attributes like `on:click={() => ...}` (the `=>` arrow) and on quoted
 * values that happen to contain `>`; both would end the tag early.
 *
 * `startIndex` may point at the `<` itself or anywhere inside the tag —
 * only characters at or after `startIndex` affect brace/quote depth.
 * Returns -1 if no depth-0, unquoted `>` is found before the end of `text`.
 */
export function findTagEnd(text: string, startIndex: number): number {
  let depth = 0;
  let quote: string | null = null;
  for (let i = startIndex; i < text.length; i++) {
    const ch = text.charAt(i);
    if (quote) {
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (ch === '{') {
      depth++;
      continue;
    }
    if (ch === '}') {
      if (depth > 0) depth--;
      continue;
    }
    if (ch === '>' && depth === 0) {
      return i;
    }
  }
  return -1;
}

/**
 * The text between the brace at `openIndex` and its match, excluding both
 * braces. Braces inside string literals and comments count too -- the depth
 * is counted naively -- so a corpus that holds them needs a different reader.
 *
 * @throws {Error} when the brace at `openIndex` never closes, so a truncated
 * slice cannot pass for a body that was read whole.
 */
export function bracedBody(text: string, openIndex: number): string {
  let depth = 1;
  for (let i = openIndex + 1; i < text.length; i++) {
    const ch = text.charAt(i);
    if (ch === '{') {
      depth++;
    } else if (ch === '}') {
      depth--;
      if (depth === 0) {
        return text.slice(openIndex + 1, i);
      }
    }
  }
  throw new Error(`unbalanced braces from index ${openIndex}`);
}

/**
 * The body of the first item `header` introduces, delimited by the brace that
 * follows the header and its match. A string header is matched literally; a
 * regular expression one is matched by its first occurrence -- searched
 * through `String.prototype.search`, which leaves `lastIndex` alone, so a
 * `g`-flagged header does not resume from wherever a previous scan left it.
 *
 * @throws {Error} when no item matches `header`, or when no brace follows it.
 * A scan anchored on an item that is not there would pass vacuously.
 */
export function itemBody(source: string, header: string | RegExp): string {
  const label = typeof header === 'string' ? header : header.source;
  const headerIndex = typeof header === 'string' ? source.indexOf(header) : source.search(header);
  if (headerIndex === -1) {
    throw new Error(`no Rust item found for header: ${label}`);
  }
  const open = source.indexOf('{', headerIndex);
  if (open === -1) {
    throw new Error(`no opening brace found for header: ${label}`);
  }
  return bracedBody(source, open);
}

/**
 * The body of `function <name>(...) { ... }` (async or not — the search
 * string is a substring of `async function <name>(...)` too), including its
 * braces. The parameter list is skipped by matching its parentheses first,
 * so an inline object type in a multi-line signature is not mistaken for the
 * end of the body; the body itself is then delimited by `bracedBody`.
 * Returns '' when no such function is declared, or when its body never closes.
 */
export function functionBody(source: string, name: string): string {
  const header = source.indexOf(`function ${name}(`);
  if (header === -1) {
    return '';
  }
  const parenIndex = source.indexOf('(', header);
  let depth = 0;
  let afterParams = -1;
  for (let i = parenIndex; i < source.length; i++) {
    const ch = source.charAt(i);
    if (ch === '(') {
      depth++;
    } else if (ch === ')') {
      depth--;
      if (depth === 0) {
        afterParams = i + 1;
        break;
      }
    }
  }
  if (afterParams === -1) {
    return '';
  }
  const bodyStart = source.indexOf('{', afterParams);
  if (bodyStart === -1) {
    return '';
  }
  try {
    return `{${bracedBody(source, bodyStart)}}`;
  } catch {
    return '';
  }
}

/**
 * Resolves `condition` to the expression it actually gates on: `condition`
 * itself when it is not a bare identifier, or the right-hand side of its
 * `$: <identifier> = <expr>;` reactive declaration when it is, so hoisting a
 * condition into such a declaration does not read as a different expression
 * than the one it was hoisted from.
 */
export function resolveConditionExpression(text: string, condition: string): string {
  const bareIdentifier = condition.match(/^([A-Za-z_$][\w$]*)$/);
  if (bareIdentifier === null) {
    return condition;
  }
  const declaration = text.match(new RegExp(`\\$:\\s*${bareIdentifier[1]}\\s*=\\s*([^;]+);`));
  return declaration?.[1]?.trim() ?? condition;
}
