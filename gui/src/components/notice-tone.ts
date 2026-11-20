import type { IconName } from '../icons/icon-paths';
import type { Notice } from '../notices/notice-feed';
import { banners } from '../ui-messages';

const TONE_ICONS: Readonly<Record<Notice['tone'], IconName>> = {
  info: 'info',
  warning: 'triangle-exclamation-point',
  error: 'circle-exclamation-point',
};

/**
 * The icon a notice of `tone` wears and the word that names it, so a tone is
 * never told by colour alone.
 * @param tone the notice's tone
 */
export function noticeToneMark(tone: Notice['tone']): { icon: IconName; label: string } {
  return { icon: TONE_ICONS[tone], label: banners.noticeTones[tone] };
}
