<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  // `dirty`: there's typing to lose, so Esc or a click outside asks first.
  let { title, onclose, children, footer, dirty = false, wide = false }: { title: string; onclose: () => void; children: Snippet; footer?: Snippet; dirty?: boolean; wide?: boolean } = $props();

  // window.confirm() doesn't show in the macOS app, so the second Esc or
  // click within a few seconds discards.
  let confirming = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  function dismiss() {
    if (!dirty || confirming) { clearTimeout(timer); onclose(); return; }
    confirming = true;
    timer = setTimeout(() => (confirming = false), 4000);
  }

  let box: HTMLDivElement | undefined = $state();
  // Start typing in the first field, and give focus back when the dialog closes.
  onMount(() => {
    const before = document.activeElement as HTMLElement | null;
    box?.querySelector<HTMLElement>(".body :is(input, textarea, select):not(:disabled)")?.focus();
    return () => before?.focus?.();
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && !e.isComposing && dismiss()} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="backdrop" onclick={(e) => e.target === e.currentTarget && dismiss()}>
  <div class="modal" class:wide role="dialog" aria-modal="true" aria-label={title} bind:this={box}>
    <div class="row"><h2 class="grow">{title}</h2><button class="ghost" onclick={onclose} aria-label="Close" title="Close (Esc)">✕</button></div>
    <div class="body">{@render children()}</div>
    {#if confirming}<div class="discard" role="alert">You have unsaved typing. Press Esc or click outside again to discard it.</div>{/if}
    {#if footer}<div class="foot">{@render footer()}</div>{/if}
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, .5); display: grid; place-items: center; z-index: 40; }
  .modal { width: min(620px, calc(100vw - 32px)); max-height: calc(100vh - 60px); overflow: auto; background: var(--panel);
    border: 1px solid var(--border); border-radius: 10px; padding: 16px 18px; }
  .modal.wide { width: min(980px, calc(100vw - 32px)); }
  h2 { margin: 0; font-size: 16px; }
  .body { margin-top: 8px; }
  .discard { margin-top: 12px; color: var(--yellow); font-size: 12px; }
  .foot { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
</style>
