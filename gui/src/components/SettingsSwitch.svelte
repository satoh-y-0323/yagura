<script lang="ts">
  /**
   * The one switch control the Settings panel's tabs share: state, label and
   * side text all come from its props, and a press is reported through
   * `onToggle`. No IPC, no decision and no text of its own -- the tab that
   * uses it owns all three.
   */
  export let checked: boolean;
  export let label: string;
  export let text: string;
  export let disabled: boolean;
  export let onToggle: () => void;
</script>

<!-- eslint-disable-next-line no-restricted-syntax -- the switch is named by `label`, which differs from its side text, so it is not an IconButton -->
<button
  type="button"
  role="switch"
  class="settings-switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  on:click={onToggle}
>
  <span class="settings-switch__track" aria-hidden="true"></span>
  {text}
</button>

<style>
  .settings-switch {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: 0;
    border: none;
    background: none;
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
    cursor: pointer;
  }

  .settings-switch__track {
    position: relative;
    flex: 0 0 34px;
    width: 34px;
    height: 18px;
    border-radius: 9px;
    background-color: var(--color-border);
    transition: background-color 0.15s;
  }

  .settings-switch__track::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background-color: var(--color-text-primary);
    transition: left 0.15s;
  }

  .settings-switch[aria-checked='true'] .settings-switch__track {
    background-color: var(--color-primary);
  }

  .settings-switch[aria-checked='true'] .settings-switch__track::after {
    left: 18px;
  }

  .settings-switch:focus-visible {
    outline: 2px solid var(--color-primary);
    outline-offset: 2px;
  }

  .settings-switch:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
