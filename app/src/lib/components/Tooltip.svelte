<script lang="ts">
  // Hover descriptions for the whole app. A button's or label's `title`
  // shows here, styled and after a short pause, instead of the system's slow
  // plain one; so does `data-tip`. Keyboard focus shows it too.
  // While a tip is up its `title` is taken off, so the system one stays away,
  // and kept as `aria-description` for screen readers; it goes back when the
  // tip hides, so an element is never left with stale or missing text.
  import { onMount, tick } from "svelte";

  const DELAY = 450;
  // Not every `[title]`: an iframe's title is its accessible name, and once
  // the pointer is inside a frame this window gets no events to hide a tip.
  const TIPPED = "button[title], label[title], a[title], [data-tip]";

  let text = $state("");
  let x = $state(0);
  let y = $state(0);
  let below = $state(true);
  let tipEl: HTMLDivElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let target: Element | null = null;
  let restore: (() => void) | null = null;

  /** The element's description, taken off `title` until the tip hides. */
  function take(el: Element): string {
    const title = el.getAttribute("title");
    if (!title) return el.getAttribute("data-tip") ?? "";
    el.removeAttribute("title");
    const described = el.hasAttribute("aria-description");
    if (!described) el.setAttribute("aria-description", title);
    restore = () => {
      if (!el.hasAttribute("title")) el.setAttribute("title", title);
      if (!described) el.removeAttribute("aria-description");
    };
    return title;
  }

  function hide() {
    clearTimeout(timer);
    text = "";
    restore?.();
    restore = null;
    target = null;
  }

  async function place(el: Element) {
    const r = el.getBoundingClientRect();
    below = r.bottom + 80 < window.innerHeight;
    x = r.left + r.width / 2;
    y = below ? r.bottom + 6 : r.top - 6;
    // Keep it on screen once it's drawn and has a size.
    await tick();
    if (!tipEl) return;
    const w = tipEl.offsetWidth;
    x = Math.min(Math.max(x, w / 2 + 8), window.innerWidth - w / 2 - 8);
  }

  function show(from: EventTarget | null) {
    const el = (from as Element | null)?.closest?.(TIPPED) ?? null;
    if (el === target) return;
    hide();
    if (!el || el.closest(".xterm, iframe")) return;
    const tip = take(el);
    if (!tip) {
      restore?.();
      restore = null;
      return;
    }
    target = el;
    timer = setTimeout(() => {
      if (target !== el || !el.isConnected) return;
      text = tip;
      place(el);
    }, DELAY);
  }

  onMount(() => {
    const over = (e: PointerEvent) => show(e.target);
    // Tabbing to a button shows its tip; a click's focus doesn't.
    const focus = (e: FocusEvent) => {
      if ((e.target as Element | null)?.matches?.(":focus-visible")) show(e.target);
    };
    // Leaving the window (relatedTarget null) hides it.
    const out = (e: PointerEvent) => {
      if (!e.relatedTarget) hide();
    };
    window.addEventListener("pointerover", over);
    window.addEventListener("pointerout", out);
    window.addEventListener("focusin", focus);
    window.addEventListener("focusout", hide);
    window.addEventListener("pointerdown", hide, true);
    window.addEventListener("keydown", hide, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);
    return () => {
      hide();
      window.removeEventListener("pointerover", over);
      window.removeEventListener("pointerout", out);
      window.removeEventListener("focusin", focus);
      window.removeEventListener("focusout", hide);
      window.removeEventListener("pointerdown", hide, true);
      window.removeEventListener("keydown", hide, true);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("blur", hide);
    };
  });
</script>

{#if text}
  <div class="tip" class:above={!below} role="tooltip" bind:this={tipEl} style:left="{x}px" style:top="{y}px">{text}</div>
{/if}

<style>
  .tip {
    position: fixed; z-index: 100; transform: translateX(-50%); max-width: 320px; pointer-events: none;
    background: var(--panel-2); color: var(--text); border: 1px solid var(--border-strong, var(--border));
    border-radius: 7px; padding: 6px 9px; font-size: 12px; line-height: 1.4; white-space: pre-wrap;
    box-shadow: 0 6px 20px rgba(0, 0, 0, .28); animation: in .12s ease-out;
  }
  .tip.above { transform: translate(-50%, -100%); }
  @keyframes in { from { opacity: 0; } to { opacity: 1; } }
</style>
