import { messagesEn, type TitleParts as TitlePartsSource } from './messages-en';
import type { UiText, UiTextOf } from '../display/ui-text';
import type { DisplayBlock, DisplayLine } from '../display/untrusted-text';

/**
 * The shape of a locale, derived from the English resource: a second
 * locale that is missing a key is a type error, not a runtime fallback.
 */
export type MessagesSource = typeof messagesEn;

/** A resolved locale: the resource's shape with the text it yields typed as `UiText`. */
export type Messages = UiTextOf<MessagesSource>;

/** A sentence split around the name it holds, as a resolved locale yields it. */
export type TitleParts = UiTextOf<TitlePartsSource>;

/**
 * Locale selection is a policy, kept in this one place. English only for now;
 * a future locale plugs in here.
 */
export function resolveMessages(): Messages {
  // The one place text becomes `UiText`: what the resource yields is the UI's own words.
  return messagesEn as Messages;
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
