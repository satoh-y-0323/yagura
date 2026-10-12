<script lang="ts">
  import { chat, describeAttentionBadge, nav, plans } from '../ui-messages';
  import type { PlansAttention } from '../plans/plan-board-model';
  import { themeMarkOf } from './theme-mark';
  import IconButton from './IconButton.svelte';
  // The small-size glyph the .ico also uses at 16-32px; the master icon blurs at rail size.
  import appMark from '../../src-tauri/icons/icon-small.svg';

  /**
   * The rail carries navigation only: which panel
   * is visible, the app theme, and the "needs you" count. It owns no IPC and
   * no store: the count and the theme wiring are both derived
   * upstream and handed in as props.
   */
  export let attentionCount: number;
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
</script>

<nav class="app-rail">
  <img class="app-rail__mark" src={appMark} alt="" aria-hidden="true" />

  <IconButton class="app-rail__button app-rail__button--active" label={nav.windows}>
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
    {#if attentionCount > 0}
      <span
        class="app-rail__badge"
        data-tone="attention"
        aria-label={describeAttentionBadge(attentionCount)}>{attentionCount}</span
      >
    {/if}
  </IconButton>

  <IconButton
    class="app-rail__button"
    label={nav.plans}
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
      <span class="app-rail__badge" data-tone="attention" aria-label={plansAttentionText}></span>
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
      <span class="app-rail__badge" data-tone="attention" aria-label={chat.needsYouBadge}></span>
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
      title={themeMark.tooltip}
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
    flex: 0 0 48px;
    width: 48px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) 0;
    background-color: var(--color-background);
    border-right: 1px solid var(--color-border);
  }

  .app-rail__mark {
    flex: 0 0 auto;
    width: 30px;
    height: 30px;
  }

  :global(.app-rail__button) {
    position: relative;
    flex: 0 0 auto;
    width: 34px;
    height: 34px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-left: 2px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--color-text-secondary);
    cursor: pointer;
  }

  :global(.app-rail__button:disabled) {
    opacity: 0.4;
    cursor: not-allowed;
  }

  :global(.app-rail__button[aria-pressed='true']) {
    background-color: var(--color-surface);
    color: var(--color-primary);
  }

  :global(.app-rail__button--active) {
    background-color: var(--color-surface);
    border-left-color: var(--color-primary);
    color: var(--color-primary);
  }

  .app-rail__icon {
    width: 18px;
    height: 18px;
  }

  .app-rail__badge {
    position: absolute;
    top: 2px;
    right: 2px;
    min-width: 16px;
    height: 16px;
    padding: 0 3px;
    border-radius: 999px;
    font-size: 10px;
    line-height: 16px;
    text-align: center;
  }

  .app-rail__spacer {
    flex: 1 1 auto;
  }

  .app-rail__divider {
    flex: 0 0 auto;
    width: 24px;
    height: 1px;
    background-color: var(--color-border);
  }

  /* The hidden select cannot show its own focus ring, so the seat shows it,
     for keyboard focus only. */
  .app-rail__theme:has(.theme-select:focus-visible) {
    outline: 2px solid var(--color-primary);
    outline-offset: 2px;
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
    font-size: var(--font-size-sm);
  }
</style>
