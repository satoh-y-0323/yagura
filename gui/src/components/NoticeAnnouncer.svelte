<script lang="ts">
  /**
   * The page's two live regions, present from the start so a screen reader is
   * listening before the first announcement. Display only: each run of the
   * feed hands over what it newly reads out, and only the text inside the
   * regions changes. A fresh node per announcement makes the same sentence
   * twice in a row read twice.
   */
  import type { UiText } from '../display/ui-text';
  import type { NoticeFeedOutput } from '../notices/notice-feed';
  import { banners } from '../ui-messages';

  export let announce: NoticeFeedOutput['announce'];

  // `null` draws an empty region, as nothing has been read out yet.
  let polite: UiText | null = null;
  let assertive: UiText | null = null;
  let politeRevision = 0;
  let assertiveRevision = 0;

  $: take(announce);

  function take(next: NoticeFeedOutput['announce']): void {
    if (next.polite !== null) {
      polite = next.polite;
      politeRevision += 1;
    }
    if (next.assertive !== null) {
      assertive = next.assertive;
      assertiveRevision += 1;
    }
  }
</script>

<div class="notice-announcer">
  <h2>{banners.screenReaderAnnouncements}</h2>
  <div aria-live="polite">
    {#key politeRevision}<p>{polite}</p>{/key}
  </div>
  <div aria-live="assertive">
    {#key assertiveRevision}<p>{assertive}</p>{/key}
  </div>
</div>

<style>
  .notice-announcer {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .notice-announcer h2,
  .notice-announcer p {
    margin: 0;
  }
</style>
