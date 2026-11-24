// `yagura/no-brand-forging` checks an assertion here only for keeping one set of foreign brands
// (`DisplayLine`, `DisplayBlock`), not where they sit: add none touching them but `as Messages`.
import { messagesEn, type HeadlineWords, type TitleWords } from './messages-en';
import type { UiText, UiTextOf } from '../display/ui-text';
import type { DisplayBlock, DisplayLine } from '../display/untrusted-text';
import type { HeadlineParts, HeadlineParty } from '../session/approval-types';

/**
 * The shape of a locale, derived from the English resource: a second
 * locale that is missing a key is a type error, not a runtime fallback.
 */
export type MessagesSource = typeof messagesEn;

/** A resolved locale: the resource's shape with the text it yields typed as `UiText`. */
export type Messages = UiTextOf<MessagesSource>;

/** A sentence split around the name it holds, so a card can cut the name short and keep the rest. */
export interface TitleParts {
  before: UiText;
  name: DisplayLine;
  after: UiText;
  /** The parts joined: the whole sentence, made where the parts are. */
  sentence: UiText;
}

/**
 * Locale selection is a policy, kept in this one place. English only for now;
 * a future locale plugs in here.
 */
export function resolveMessages(): Messages {
  // The one place text becomes `UiText`: what the resource yields is the UI's own words.
  return messagesEn as Messages;
}

/**
 * Joins a locale's words around a name into the parts a card draws and the sentence they make.
 *
 * @param words the words before and after the name, from a resolved locale
 * @param name the name, prepared for display
 */
export function titleParts(words: UiTextOf<TitleWords>, name: DisplayLine): TitleParts {
  const { before, after } = words;
  return { before, name, after, sentence: `${before}${name}${after}` as UiText };
}

/**
 * Builds an approval heading from a locale's words and the parties it names, in the order it reads.
 *
 * @param words the verb and the words after a tool, from a resolved locale
 * @param from who asks
 * @param to what it would act on; `null` when it names nothing
 * @param tool a tool this build does not know, named rather than guessed at; else `null`
 */
export function headlineParts(
  words: UiTextOf<HeadlineWords>,
  from: HeadlineParty,
  to: HeadlineParty | null,
  tool: DisplayLine | null
): HeadlineParts {
  const { verb, toolLink } = words;
  // The sentence is the parts joined; it does not tell a name from words.
  const sentence =
    `${partyText(from)}${verb}${tool ?? ''}${toolLink}${to === null ? '' : partyText(to)}` as UiText;
  return { from, verb, tool, toolLink, to, sentence };
}

function partyText(party: HeadlineParty): UiText | DisplayLine {
  return 'name' in party ? party.name : party.words;
}

/** True when a parameter of type `P` accepts text that was not prepared for display. */
export type AcceptsUnprepared<P> = 0 extends 1 & P
  ? true
  : unknown extends P
    ? true
    : true extends MemberAcceptsUnprepared<P>
      ? true
      : false;

type MemberAcceptsUnprepared<P> = P extends DisplayBlock
  ? true
  : P extends UiText | DisplayLine
    ? false
    : string extends P
      ? true
      : P extends string
        ? Record<never, never> extends Record<P, 1>
          ? true
          : false
        : P extends number | bigint | boolean | symbol | null | undefined
          ? false
          : P extends (...args: never[]) => unknown
            ? false
            : P extends readonly (infer E)[]
              ? AcceptsUnprepared<E>
              : P extends Iterable<infer E>
                ? AcceptsUnprepared<E>
                : P extends Date
                  ? false
                  : P extends object
                    ? AcceptsUnprepared<P[keyof P]>
                    : true;

/** The dotted paths of the `messagesEn` functions with a parameter that accepts unprepared text. */
export type UnpreparedParams<T, Path extends string = ''> = T extends string
  ? never
  : T extends (...args: infer A) => unknown
    ? true extends AcceptsUnprepared<A[number]>
      ? Path
      : never
    : T extends readonly (infer E)[]
      ? UnpreparedParams<E, `${Path}[]`>
      : T extends object
        ? {
            [K in keyof T & string]: UnpreparedParams<T[K], Path extends '' ? K : `${Path}.${K}`>;
          }[keyof T & string]
        : never;

/** A type error that names the paths in `T` unless `T` is `never`. */
export type NoUnpreparedParams<T extends never> = T;

/** The paths of the `messagesEn` functions that still accept unprepared text. */
export type UnpreparedMessageParams = UnpreparedParams<MessagesSource>;

/** Fails to type-check, naming the paths, while any `messagesEn` function accepts unprepared text. */
export type MessagesTextContract = NoUnpreparedParams<UnpreparedMessageParams>;
