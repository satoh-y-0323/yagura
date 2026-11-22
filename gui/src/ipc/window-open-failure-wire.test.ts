/**
 * The key names a command failure travels under are written twice: in the
 * core's serde attributes and in this layer's narrowing. Both sides build
 * their own fixtures, so a spelling that drifted would leave both green while
 * the real path lost the field. The names are therefore read out of the Rust
 * source here and used verbatim against the boundary, rather than restated.
 * The same source decides which commands reach the daemon, and so which ones
 * this layer may call through the daemon-command door.
 */
import { describe, it, expect } from 'vitest';

import commandsSource from '../../src-tauri/src/commands.rs?raw';
import { bracedBody, itemBody } from '../test-utils/markup-scan';
import { narrowCommandFailure, type CommandFailure } from './session-boundary';

/** The Rust item whose serde representation this file reads. */
const FAILURE_STRUCT_HEADER = 'pub struct CommandFailure {';

/** One tagged arm of the wire enum, with the keys every arm shares folded in. */
interface WireVariant {
  /** The value the tag key carries for this arm. */
  tagValue: string;
  /** Keys always present on the wire. */
  required: string[];
  /** Keys the wire may omit or send as null. */
  optional: string[];
}

/** The serde representation of the failure: a tagged enum flattened beside shared keys. */
interface WireShape {
  /** The key carrying the arm's name. */
  tagKey: string;
  /** Keys every arm carries, always present on the wire. */
  shared: string[];
  variants: WireVariant[];
}

/** One named field of a Rust struct or struct variant. */
interface RustField {
  name: string;
  type: string;
  /** Whether `#[serde(flatten)]` stands above it. */
  flattened: boolean;
}

/** The arguments of the `#[serde(...)]` attribute directly above the item at `headerIndex`. */
function serdeAttributeArgs(source: string, headerIndex: number, label: string): string {
  const match = /#\[serde\(([^\]]*)\)\]\s*$/.exec(source.slice(0, headerIndex));
  if (match === null) {
    throw new Error(`no serde attribute directly above: ${label}`);
  }
  return match[1] ?? '';
}

/** `UpperCamelCase` under serde's `snake_case` renaming. */
function snakeCase(name: string): string {
  return name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
}

/** The named fields a struct body declares, each with whether it is flattened. */
function rustFields(body: string): RustField[] {
  const fields: RustField[] = [];
  const fieldLine =
    /^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?([a-z_][A-Za-z0-9_]*)[ \t]*:[ \t]*([^,\n]+),[ \t]*$/gm;
  let previousEnd = 0;
  let match: RegExpExecArray | null;
  while ((match = fieldLine.exec(body)) !== null) {
    const attributes = body.slice(previousEnd, match.index);
    fields.push({
      name: match[1] ?? '',
      type: (match[2] ?? '').trim(),
      flattened: /#\[serde\(\s*flatten\s*\)\]/.test(attributes),
    });
    previousEnd = match.index + match[0].length;
  }
  return fields;
}

/** The fields split by whether the wire may omit them. */
function splitByPresence(fields: RustField[]): { required: string[]; optional: string[] } {
  return {
    required: fields.filter((field) => !field.type.startsWith('Option<')).map((f) => f.name),
    optional: fields.filter((field) => field.type.startsWith('Option<')).map((f) => f.name),
  };
}

/** The index of the enum named `typeName`, whatever its visibility. */
function enumHeaderIndex(source: string, typeName: string): number {
  const header = new RegExp(
    `^[ \\t]*(?:pub(?:\\([^)]*\\))?[ \\t]+)?enum[ \\t]+${typeName}[ \\t]*\\{`,
    'm'
  );
  const index = source.search(header);
  if (index === -1) {
    throw new Error(`no Rust enum found named: ${typeName}`);
  }
  return index;
}

/**
 * The serde representation of the failure `structHeader` introduces: its one
 * flattened field names the internally tagged enum of kinds, and every other
 * field is a key each arm carries beside the kind's own.
 */
function wireShapeOf(source: string, structHeader: string): WireShape {
  const fields = rustFields(itemBody(source, structHeader));
  const flattened = fields.filter((field) => field.flattened);
  if (flattened.length !== 1) {
    throw new Error(
      `expected one flattened field under ${structHeader}, found ${flattened.length}`
    );
  }
  const kindType = flattened[0]?.type ?? '';
  const sharedFields = splitByPresence(fields.filter((field) => !field.flattened));
  if (sharedFields.optional.length > 0) {
    throw new Error(`a key every arm carries may not be omitted: ${sharedFields.optional.join()}`);
  }

  const enumIndex = enumHeaderIndex(source, kindType);
  const args = serdeAttributeArgs(source, enumIndex, kindType);
  const tagKey = /\btag\s*=\s*"([^"]+)"/.exec(args)?.[1];
  const renameAll = /\brename_all\s*=\s*"([^"]+)"/.exec(args)?.[1];
  if (tagKey === undefined) {
    throw new Error(`the serde attribute above ${kindType} names no tag`);
  }
  if (renameAll !== 'snake_case') {
    throw new Error(`unsupported rename_all above ${kindType}: ${String(renameAll)}`);
  }
  const body = bracedBody(source, source.indexOf('{', enumIndex));
  const variants: WireVariant[] = [];
  const variantHeader = /^[ \t]*([A-Z][A-Za-z0-9_]*)[ \t]*([{,])/gm;
  let match: RegExpExecArray | null;
  while ((match = variantHeader.exec(body)) !== null) {
    const name = match[1] ?? '';
    const own =
      match[2] === '{'
        ? splitByPresence(rustFields(bracedBody(body, match.index + match[0].length - 1)))
        : { required: [], optional: [] };
    variants.push({
      tagValue: snakeCase(name),
      required: [...own.required, ...sharedFields.required],
      optional: own.optional,
    });
  }
  return { tagKey, shared: sharedFields.required, variants };
}

/** The distinct value sent under `key`, so its landing place can be found. */
function sampleFor(key: string): string {
  return `value-sent-under-${key}`;
}

/** The string values a narrowed failure ended up carrying, under any name. */
function carriedStrings(failure: CommandFailure): Set<string> {
  const record: Record<string, unknown> = { ...failure };
  return new Set(
    Object.values(record).filter((value): value is string => typeof value === 'string')
  );
}

/** A wire object built from `variant`, with every key it declares. */
function wireObject(shape: WireShape, variant: WireVariant): Record<string, unknown> {
  const object: Record<string, unknown> = { [shape.tagKey]: variant.tagValue };
  for (const key of [...variant.required, ...variant.optional]) {
    object[key] = sampleFor(key);
  }
  return object;
}

/** The same object with `key` renamed, which no side is supposed to read. */
function withKeyRenamed(object: Record<string, unknown>, key: string): Record<string, unknown> {
  const renamed: Record<string, unknown> = {};
  for (const [name, value] of Object.entries(object)) {
    renamed[name === key ? `${name}_renamed` : name] = value;
  }
  return renamed;
}

/**
 * Every way `shape`'s key names and the boundary's disagree: a key the
 * boundary does not read, an arm it does not recognize, or a key it reads
 * from a spelling `shape` never uses. Empty when the two agree.
 */
function disagreementsWithBoundary(shape: WireShape): string[] {
  const problems: string[] = [];
  for (const variant of shape.variants) {
    const whole = wireObject(shape, variant);
    let narrowed: CommandFailure;
    try {
      narrowed = narrowCommandFailure(whole);
    } catch (error) {
      problems.push(`${variant.tagValue}: refused by the boundary (${String(error)})`);
      continue;
    }
    if (narrowed.kind !== variant.tagValue) {
      problems.push(`${variant.tagValue}: narrowed to the arm "${narrowed.kind}"`);
      continue;
    }
    const carried = carriedStrings(narrowed);
    for (const key of [...variant.required, ...variant.optional]) {
      if (!carried.has(sampleFor(key))) {
        problems.push(`${variant.tagValue}.${key}: the value under this key did not arrive`);
      }
    }
    for (const key of variant.required) {
      let refused = false;
      try {
        narrowCommandFailure(withKeyRenamed(whole, key));
      } catch {
        refused = true;
      }
      if (!refused) {
        problems.push(
          `${variant.tagValue}.${key}: accepted although this key was spelled otherwise`
        );
      }
    }
    for (const key of variant.optional) {
      const stillCarried = carriedStrings(narrowCommandFailure(withKeyRenamed(whole, key)));
      if (stillCarried.has(sampleFor(key))) {
        problems.push(`${variant.tagValue}.${key}: read from a spelling the Rust side never sends`);
      }
    }
    const tagMoved = narrowCommandFailure(withKeyRenamed(whole, shape.tagKey));
    if (tagMoved.kind === variant.tagValue) {
      problems.push(`${variant.tagValue}: the arm survived the tag key being spelled otherwise`);
    }
  }
  return problems;
}

/**
 * Read inside each test rather than at module load, so a shape this reader no
 * longer finds fails the tests that read it and leaves the others standing.
 */
function realWireShape(): WireShape {
  return wireShapeOf(commandsSource, FAILURE_STRUCT_HEADER);
}

describe('the failure wire shape is read out of the Rust source', () => {
  it('reads a non-empty source (sanity check against a vacuous pass)', () => {
    expect(commandsSource.length).toBeGreaterThan(0);
  });

  it('finds a tag key, a key every arm shares, and more than one tagged arm', () => {
    const shape = realWireShape();
    expect(shape.tagKey.length).toBeGreaterThan(0);
    expect(shape.shared.length).toBeGreaterThan(0);
    expect(shape.variants.length).toBeGreaterThan(1);
  });

  it('finds an arm that carries a key of its own beside the shared ones', () => {
    const shape = realWireShape();
    const ownKeys = shape.variants.filter(
      (variant) => variant.required.length + variant.optional.length > shape.shared.length
    );
    expect(ownKeys.length).toBeGreaterThan(0);
  });

  it('finds exactly one arm that may omit a key', () => {
    const withOptional = realWireShape().variants.filter((variant) => variant.optional.length > 0);
    expect(withOptional).toHaveLength(1);
  });
});

describe('the boundary reads the failure under the key names the core sends', () => {
  it('agrees with the Rust source on every key name and arm name', () => {
    expect(disagreementsWithBoundary(realWireShape())).toEqual([]);
  });
});

/** A failure struct around `kindArms`, flattened beside `sharedFields`. */
function failureSource(
  kindArms: string[],
  sharedFields: string[] = ['    text: String,'],
  tag = 'kind'
): string {
  return [
    '#[derive(Debug, serde::Serialize)]',
    'pub struct CommandFailure {',
    '    #[serde(flatten)]',
    '    kind: FailureKind,',
    ...sharedFields,
    '}',
    '',
    '#[derive(Debug, serde::Serialize)]',
    `#[serde(tag = "${tag}", rename_all = "snake_case")]`,
    'enum FailureKind {',
    ...kindArms,
    '}',
  ].join('\n');
}

const REJECTED_ARM = [
  '    /// The daemon answered the request with an error frame.',
  '    Rejected {',
  '        code: String,',
  '        message: String,',
  '        #[serde(skip_serializing_if = "Option::is_none")]',
  '        rejected_argument: Option<String>,',
  '    },',
];
const LOCAL_ARM = ['    Local,'];
const UNSUPPORTED_ARM = ['    Unsupported {', '        request_type: String,', '    },'];

/**
 * The check above is only worth its green if a disagreement would show up in
 * it. Rather than breaking the real source, the same reader is handed
 * fragments that disagree on purpose.
 */
describe('the agreement check answers for a source that disagrees', () => {
  const DISAGREEING_SOURCES: Array<[string, string]> = [
    [
      'a field spelled differently',
      failureSource([
        '    Rejected {',
        '        code: String,',
        '        message: String,',
        '        rejected_arg: Option<String>,',
        '    },',
        ...LOCAL_ARM,
      ]),
    ],
    [
      'an arm named differently',
      failureSource([
        '    Refused {',
        '        code: String,',
        '        message: String,',
        '        rejected_argument: Option<String>,',
        '    },',
        ...LOCAL_ARM,
      ]),
    ],
    [
      'the tag under another key',
      failureSource([...REJECTED_ARM, ...LOCAL_ARM], undefined, 'variant'),
    ],
    [
      'a field the boundary never reads',
      failureSource([
        '    Rejected {',
        '        code: String,',
        '        message: String,',
        '        rejected_argument: Option<String>,',
        '        retry_after_ms: String,',
        '    },',
        ...LOCAL_ARM,
      ]),
    ],
    [
      'the shared sentence under another name',
      failureSource([...REJECTED_ARM, ...LOCAL_ARM], ['    sentence: String,']),
    ],
  ];

  it.each(DISAGREEING_SOURCES)('reports a disagreement for %s', (_label, source) => {
    const shape = wireShapeOf(source, FAILURE_STRUCT_HEADER);

    expect(disagreementsWithBoundary(shape).length).toBeGreaterThan(0);
  });

  it('reports nothing for a fragment that restates the real shape', () => {
    const faithful = failureSource([...REJECTED_ARM, ...LOCAL_ARM, ...UNSUPPORTED_ARM]);
    const shape = wireShapeOf(faithful, FAILURE_STRUCT_HEADER);

    expect(shape.variants.map((variant) => variant.tagValue)).toEqual([
      'rejected',
      'local',
      'unsupported',
    ]);
    expect(disagreementsWithBoundary(shape)).toEqual([]);
  });
});

/**
 * A command reaches the daemon only through the pool Tauri hands it as state,
 * and the pool's methods fail with `IpcError`. So every command that takes the
 * pool and returns a `Result` fails with the one shape the boundary narrows.
 * Tauri lets a command pick any error type, so no type check stops a
 * signature from choosing a string instead, which flattens the failure's kind
 * into its sentence before the WebView sees it.
 */
const POOL_PARAMETER =
  /State<'[A-Za-z_]+,(?:[A-Za-z0-9_]+::)*Arc<(?:[A-Za-z0-9_]+::)*ConnectionPool>>/;
const COMMAND_FAILURE_TYPE = 'CommandFailure';
const COMMAND_ATTRIBUTE = /#\[tauri::command\b/g;

/** What the rule reads of one `#[tauri::command]` function. */
interface CommandSignature {
  name: string;
  takesPool: boolean;
  /** The error type of the `Result` it returns; `null` when it returns none. */
  errorType: string | null;
}

function squeezed(text: string): string {
  return text.replace(/\s+/g, '');
}

/**
 * The index of the bracket that closes the one at `openIndex`, counting
 * `open` and `close` naively.
 *
 * @throws {Error} when it never closes, so a truncated signature cannot pass.
 */
function closingIndex(text: string, openIndex: number, open: string, close: string): number {
  let depth = 0;
  for (let i = openIndex; i < text.length; i++) {
    const ch = text.charAt(i);
    if (ch === open) {
      depth++;
    } else if (ch === close) {
      depth--;
      if (depth === 0) {
        return i;
      }
    }
  }
  throw new Error(`unbalanced ${open} from index ${openIndex}`);
}

/** The items of a comma-separated type list, split only outside every bracket. */
function topLevelItems(list: string): string[] {
  const items: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < list.length; i++) {
    const ch = list.charAt(i);
    if (ch === '<' || ch === '(' || ch === '[') {
      depth++;
    } else if (ch === '>' || ch === ')' || ch === ']') {
      depth--;
    } else if (ch === ',' && depth === 0) {
      items.push(list.slice(start, i));
      start = i + 1;
    }
  }
  items.push(list.slice(start));
  return items;
}

/** The error type of a whitespace-free return type that is a `Result`, else `null`. */
function resultErrorType(returnType: string): string | null {
  const match = /^(?:[A-Za-z0-9_]+::)*Result<(.*)>$/.exec(returnType);
  if (match === null) {
    return null;
  }
  const items = topLevelItems(match[1] ?? '');
  if (items.length !== 2) {
    throw new Error(`a Result with ${String(items.length)} type arguments: ${returnType}`);
  }
  return items[1] ?? null;
}

/** Every `#[tauri::command]` function in `source`, in source order. */
function commandSignatures(source: string): CommandSignature[] {
  const signatures: CommandSignature[] = [];
  for (const attribute of source.matchAll(COMMAND_ATTRIBUTE)) {
    const rest = source.slice(attribute.index);
    const header = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(/.exec(rest);
    if (header === null) {
      throw new Error(`no function follows the command attribute at index ${attribute.index}`);
    }
    const openParen = attribute.index + header.index + header[0].length - 1;
    const closeParen = closingIndex(source, openParen, '(', ')');
    const bodyOpen = source.indexOf('{', closeParen);
    if (bodyOpen === -1) {
      throw new Error(`no body follows the command ${header[1] ?? ''}`);
    }
    const tail = squeezed(source.slice(closeParen + 1, bodyOpen));
    signatures.push({
      name: header[1] ?? '',
      takesPool: POOL_PARAMETER.test(squeezed(source.slice(openParen + 1, closeParen))),
      errorType: tail.startsWith('->') ? resultErrorType(tail.slice(2)) : null,
    });
  }
  return signatures;
}

/** Whether the rule covers `signature`: it takes the pool and returns a `Result`. */
function inRuleScope(signature: CommandSignature): boolean {
  return signature.takesPool && signature.errorType !== null;
}

/**
 * Every command the rule covers whose error type is not the failure shape, as
 * `name: error type`. Empty when the rule holds.
 */
function poolCommandsFailingOtherwise(source: string): string[] {
  return commandSignatures(source)
    .filter(inRuleScope)
    .filter((signature) => signature.errorType?.split('::').pop() !== COMMAND_FAILURE_TYPE)
    .map((signature) => `${signature.name}: ${signature.errorType ?? ''}`);
}

describe('every command that reaches the daemon fails with the failure shape', () => {
  it('finds commands the rule covers in the Rust source (sanity check against a vacuous pass)', () => {
    const signatures = commandSignatures(commandsSource);
    expect(signatures.filter(inRuleScope).length).toBeGreaterThan(1);
    expect(new Set(signatures.map((signature) => signature.name)).size).toBe(signatures.length);
  });

  it('finds no command that takes the pool and returns a Result failing otherwise', () => {
    expect(poolCommandsFailingOtherwise(commandsSource)).toEqual([]);
  });
});

/** The same check, handed fragments that break or keep the rule on purpose. */
describe('the failure shape rule answers for a source that breaks it', () => {
  const BREAKING_SOURCES: Array<[string, string, string]> = [
    [
      'a pool command that fails with a string',
      [
        '#[tauri::command(rename_all = "snake_case")]',
        'pub async fn list_windows(',
        '    app: AppHandle,',
        "    pool: State<'_, Arc<ConnectionPool>>,",
        ') -> Result<Vec<yagura_protocol::WindowInfo>, String> {',
        '    pool.list_windows(&app).await.map_err(|error| error.to_string())',
        '}',
      ].join('\n'),
      'list_windows: String',
    ],
    [
      'a pool command whose success type holds a comma',
      [
        '#[tauri::command]',
        'pub async fn next_frames(',
        '    acks: Vec<FrameAckEntry>,',
        "    pool: State<'_, Arc<ConnectionPool>>,",
        ') -> Result<Vec<(String, Frame)>, String> {',
        '    Ok(pool.frames().take().await)',
        '}',
      ].join('\n'),
      'next_frames: String',
    ],
    [
      'a pool command that fails with another named type',
      [
        '#[tauri::command(rename_all = "snake_case")]',
        'pub async fn save_roles(',
        "    pool: State<'_, Arc<ConnectionPool>>,",
        ') -> Result<(), crate::keymap::KeymapSaveFailure> {',
        '    Ok(())',
        '}',
      ].join('\n'),
      'save_roles: crate::keymap::KeymapSaveFailure',
    ],
    [
      'a one-line pool command under a further attribute',
      [
        '#[allow(clippy::too_many_arguments)]',
        '#[tauri::command(rename_all = "snake_case")]',
        "pub async fn resize(pool: State<'_, Arc<ConnectionPool>>) -> Result<(), String> {",
        '    Ok(())',
        '}',
      ].join('\n'),
      'resize: String',
    ],
  ];

  it.each(BREAKING_SOURCES)('reports %s', (_label, source, reported) => {
    expect(poolCommandsFailingOtherwise(source)).toEqual([reported]);
  });

  it('reports nothing for commands the rule keeps or does not cover', () => {
    const keeping = [
      '#[tauri::command(rename_all = "snake_case")]',
      'pub async fn open_window(',
      "    pool: State<'_, Arc<ConnectionPool>>,",
      ') -> Result<OpenWindowReply, CommandFailure> {',
      '    todo()',
      '}',
      '#[tauri::command(rename_all = "snake_case")]',
      'pub async fn reconnect_daemon(',
      "    pool: State<'_, Arc<ConnectionPool>>,",
      ') -> Result<(), crate::commands::CommandFailure> {',
      '    Ok(())',
      '}',
      '#[tauri::command(rename_all = "snake_case")]',
      'pub fn get_daemon_connection(',
      "    pool: State<'_, Arc<ConnectionPool>>,",
      ') -> Option<DaemonConnectedPayload> {',
      '    pool.daemon_connection()',
      '}',
      '#[tauri::command(rename_all = "snake_case")]',
      'pub fn set_attention_summary(',
      "    attention: State<'_, AttentionState>,",
      ') -> Result<(), String> {',
      '    Ok(())',
      '}',
    ].join('\n');

    expect(commandSignatures(keeping)).toHaveLength(4);
    expect(poolCommandsFailingOtherwise(keeping)).toEqual([]);
  });
});

/**
 * Which door a command is called through is decided by its name's type on
 * this side, and by its signature on the Rust side. The daemon-command door
 * narrows the rejection into the failure shape, so its names must be exactly
 * the commands the rule above covers; the other door hands a rejection back
 * as it came, so none of its names may be one of them.
 */
describe('the command names this layer calls agree with the Rust signatures', () => {
  /** The module that owns the two doors, loaded where each test reads it. */
  async function commandDoors(): Promise<{
    DAEMON_COMMAND_NAMES: readonly string[];
    LOCAL_COMMAND_NAMES: readonly string[];
  }> {
    return import('./command-invoke');
  }

  function namesTheRuleCovers(): Set<string> {
    return new Set(
      commandSignatures(commandsSource)
        .filter(inRuleScope)
        .map((signature) => signature.name)
    );
  }

  it('names exactly the commands that take the pool and return a Result as daemon commands', async () => {
    const { DAEMON_COMMAND_NAMES } = await commandDoors();

    expect(new Set(DAEMON_COMMAND_NAMES).size).toBe(DAEMON_COMMAND_NAMES.length);
    expect([...DAEMON_COMMAND_NAMES].sort()).toEqual([...namesTheRuleCovers()].sort());
  });

  it('names only commands the Rust source defines as local commands, none of them a daemon command', async () => {
    const { LOCAL_COMMAND_NAMES } = await commandDoors();
    const defined = new Set(commandSignatures(commandsSource).map((signature) => signature.name));
    const covered = namesTheRuleCovers();

    expect(LOCAL_COMMAND_NAMES.length).toBeGreaterThan(0);
    expect(LOCAL_COMMAND_NAMES.filter((name) => !defined.has(name))).toEqual([]);
    expect(LOCAL_COMMAND_NAMES.filter((name) => covered.has(name))).toEqual([]);
  });
});
