<script lang="ts">
  import { chat, describeAttentionBadge, nav, plans } from '../ui-messages';
  import type { WindowStatusSummary } from '../status-tone';
  import StatusBadge from './StatusBadge.svelte';
  import PlansAttentionMark from './PlansAttentionMark.svelte';
  import type { PlansAttention } from '../plans/plan-board-model';
  import { themeMarkOf } from './theme-mark';
  import IconButton from './IconButton.svelte';
  import { tooltip } from './tooltip';
  // The small-size glyph the .ico also uses at 16-32px; the master icon blurs at rail size.
  import appMark from '../../src-tauri/icons/icon-small.svg';

  /**
   * The rail carries navigation only: the work surface, which panel is
   * visible, the app theme, and the "needs you" count. It owns no IPC and
   * no store: the count and the theme wiring are both derived
   * upstream and handed in as props.
   */
  export let windowsSummary: WindowStatusSummary;
  /** Takes the keyboard back to the work surface, the current window's terminal. */
  export let onWindowsSelect: () => void;
  export let appThemeValue: string;
  export let appThemeOptions: { value: string; label: string }[];
  export let onAppThemeChange: (event: Event) => void;
  /** Whether the chat room is asking for the user; its own badge. */
  export let chatNeedsUser: boolean;
  export let onChatToggle: () => void;
  export let onPlansToggle: () => void;
  /** Whether each panel is open, shown as the pressed state of its button. */
  export let isPlansOpen: boolean;
  export let isChatOpen: boolean;
  /** What the Plans button's own badge flags, `null` for nothing. */
  export let plansAttention: PlansAttention;
  export let onSettingsToggle: () => void;
  export let isSettingsOpen: boolean;

  // The badge is a bare dot, so the button itself carries the reason as a
  // hover tooltip; an aria-label alone is never shown to a sighted user.
  $: plansAttentionText =
    plansAttention === null
      ? null
      : plans.plansAttentionBadge(plansAttention.kind === 'pending', plansAttention.count);
  $: themeMark = themeMarkOf(appThemeValue, appThemeOptions);
  // The button's own name replaces the badge's, so the count is described instead.
  $: windowsHint =
    windowsSummary.count > 0 ? describeAttentionBadge(windowsSummary.count) : undefined;
</script>

<nav class="app-rail">
  <img class="app-rail__mark" src={appMark} alt="" aria-hidden="true" />

  <!-- The work surface is always the windows until other pages exist, so this
       item is always the current place. -->
  <IconButton
    class="app-rail__button"
    label={nav.windows}
    hint={windowsHint}
    aria-current="page"
    on:click={onWindowsSelect}
  >
    <svg class="app-rail__icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <rect
        x="3"
        y="4"
        width="18"
        height="16"
        rx="2"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
      />
      <line x1="3" y1="9" x2="21" y2="9" stroke="currentColor" stroke-width="2" />
    </svg>
    {#if windowsSummary.count > 0 && windowsSummary.mark !== null}
      <span class="app-rail__badge">
        <StatusBadge status={windowsSummary.mark} variant="count" count={windowsSummary.count} />
      </span>
    {:else if windowsSummary.unknownOnly}
      <span class="app-rail__badge"><StatusBadge status="unknown" variant="icon" /></span>
    {/if}
  </IconButton>

  <IconButton
    class="app-rail__button"
    label={nav.board}
    hint={plansAttentionText ?? undefined}
    aria-pressed={isPlansOpen}
    on:click={onPlansToggle}
  >
    <svg class="app-rail__icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <rect
        x="3"
        y="4"
        width="5"
        height="16"
        rx="1"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
      />
      <rect
        x="10"
        y="4"
        width="5"
        height="10"
        rx="1"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
      />
      <rect
        x="17"
        y="4"
        width="4"
        height="13"
        rx="1"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
      />
    </svg>
    {#if plansAttention !== null}
      <span class="app-rail__plans-mark"><PlansAttentionMark attention={plansAttention} /></span>
    {/if}
  </IconButton>

  <IconButton
    class="app-rail__button"
    label={nav.chat}
    aria-pressed={isChatOpen}
    on:click={onChatToggle}
  >
    <svg class="app-rail__icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path
        d="M4 5h16v11H9l-4 4V5z"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linejoin="round"
      />
    </svg>
    {#if chatNeedsUser}
      <span class="app-rail__badge app-rail__badge--dot" aria-label={chat.needsYouBadge}></span>
    {/if}
  </IconButton>

  <div class="app-rail__spacer"></div>

  <div class="app-rail__divider"></div>

  <!-- The select stays the control (keyboard, the OS's own list) but is laid
       transparently over the icon: the rail is too narrow to show a name. -->
  <div class="app-rail__button app-rail__theme">
    <svg class="app-rail__icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      {#if themeMark.icon === 'moon'}
        <path
          d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linejoin="round"
        />
      {:else if themeMark.icon === 'sun'}
        <circle cx="12" cy="12" r="4" fill="none" stroke="currentColor" stroke-width="2" />
        <path
          d="M12 2v2.5M12 19.5V22M2 12h2.5M19.5 12H22M4.9 4.9l1.8 1.8M17.3 17.3l1.8 1.8M4.9 19.1l1.8-1.8M17.3 6.7l1.8-1.8"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        />
      {:else if themeMark.icon === 'system'}
        <circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="2" />
        <path d="M12 4a8 8 0 0 1 0 16z" fill="currentColor" />
      {:else if themeMark.icon === 'palette'}
        <path
          d="M12 3a9 9 0 1 0 0 18c1.2 0 1.8-.9 1.8-1.8 0-.5-.2-.9-.5-1.2-.3-.3-.5-.7-.5-1.2 0-1 .8-1.8 1.8-1.8H17a4 4 0 0 0 4-4C21 6.6 17 3 12 3z"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linejoin="round"
        />
        <circle cx="7.5" cy="11" r="1.3" fill="currentColor" />
        <circle cx="10" cy="7.3" r="1.3" fill="currentColor" />
        <circle cx="14.5" cy="7.3" r="1.3" fill="currentColor" />
      {/if}
    </svg>
    <!-- eslint-disable-next-line svelte/no-reactive-reassign -- bind:value write-back is unused (onAppThemeChange reads event.currentTarget.value); dropping bind: would change the reactive graph -->
    <select
      class="theme-select"
      aria-label={nav.theme}
      use:tooltip={{ text: themeMark.tooltip, announcedElsewhere: true }}
      bind:value={appThemeValue}
      on:change={onAppThemeChange}
    >
      {#each appThemeOptions as option (option.value)}
        <option value={option.value}>{option.label}</option>
      {/each}
    </select>
  </div>

  <IconButton
    class="app-rail__button"
    label={nav.settings}
    aria-pressed={isSettingsOpen}
    on:click={onSettingsToggle}
  >
    <svg class="app-rail__icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="2" />
      <path
        d="M19 12c0-.4 0-.7-.1-1.1l2-1.5-2-3.4-2.3.9c-.6-.5-1.2-.8-1.9-1.1L14.3 3h-4.6l-.4 2.8c-.7.3-1.3.6-1.9 1.1l-2.3-.9-2 3.4 2 1.5c-.1.4-.1.7-.1 1.1s0 .7.1 1.1l-2 1.5 2 3.4 2.3-.9c.6.5 1.2.8 1.9 1.1l.4 2.8h4.6l.4-2.8c.7-.3 1.3-.6 1.9-1.1l2.3.9 2-3.4-2-1.5c.1-.4.1-.7.1-1.1z"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linejoin="round"
      />
    </svg>
  </IconButton>
</nav>

<style>
  .app-rail {
    flex: 0 0 var(--rail-width);
    width: var(--rail-width);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-1_5) 0;
    background-color: var(--bg-chrome);
    border-right: var(--line-width) solid var(--line);
  }

  .app-rail__mark {
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    margin: var(--space-0_5) 0 var(--space-2);
  }

  :global(.app-rail__button) {
    position: relative;
    flex: 0 0 auto;
    width: var(--rail-button-size);
    height: var(--rail-button-size);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }

  :global(.app-rail__button:hover) {
    background-color: var(--bg-hover);
  }

  :global(.app-rail__button:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 0;
  }

  /* A panel floats over the work surface rather than replacing it, so an open
     one is shown pressed, never with the bar that marks the current place. */
  :global(.app-rail__button[aria-pressed='true']) {
    background-color: var(--bg-selected);
    color: var(--fg-default);
  }

  :global(.app-rail__button[aria-current='page']) {
    color: var(--fg-default);
  }

  /* The bar sits on the rail's own edge, so the place is not told by colour alone. */
  :global(.app-rail__button[aria-current='page']::before) {
    content: '';
    position: absolute;
    top: 7px;
    bottom: 7px;
    left: calc((var(--rail-width) - var(--rail-button-size)) / -2);
    width: var(--line-width-strong);
    border-radius: var(--line-width-strong);
    background-color: var(--fg-default);
  }

  .app-rail__icon {
    width: 18px;
    height: 18px;
  }

  .app-rail__badge {
    position: absolute;
    top: -2px;
    right: -3px;
    display: flex;
  }

  /* The chat asks for the user: a bare dot in the input-needed colour. */
  .app-rail__badge--dot {
    top: var(--space-1);
    right: var(--space-1);
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    background-color: var(--st-input);
    box-shadow: 0 0 0 var(--line-width-strong) var(--bg-chrome);
  }

  .app-rail__plans-mark {
    position: absolute;
    top: -2px;
    right: -3px;
    display: flex;
  }

  .app-rail__spacer {
    flex: 1 1 auto;
  }

  .app-rail__divider {
    flex: 0 0 auto;
    width: 24px;
    height: var(--line-width);
    background-color: var(--line);
  }

  /* The hidden select cannot show its own focus ring, so the seat shows it,
     for keyboard focus only. */
  .app-rail__theme:has(.theme-select:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 0;
  }

  /* Transparent over the icon but still hit-tested, so a click opens the
     OS's own list. That list draws itself from the `color-scheme` the tokens
     set, which is what keeps it legible. */
  .theme-select {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    padding: 0;
    border: none;
    opacity: 0;
    cursor: pointer;
    font-size: var(--text-sm);
  }
</style>
