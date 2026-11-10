<script lang="ts">
  // Hover descriptions for the whole app. Any element's `title` shows here,
  // styled and after a short pause, instead of the system's slow plain one.
  // The title moves to `data-tip` on first hover so the system one stays away;
  // when Svelte updates the title, the next hover picks up the new text.
  import { onMount } from "svelte";

  const DELAY = 450;
  let text = $state("");
  let x = $state(0);
  let y = $state(0);
  let below = $state(true);
  let tipEl: HTMLDivElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let target: Element | null = null;

  function tipOf(el: Element): string {
    const title = el.getAttribute("title");
    if (title) {
      el.setAttribute("data-tip", title);
      el.removeAttribute("title");
      // An icon-only button keeps its description for screen readers.
      if (!el.getAttribute("aria-label") && !(el.textContent ?? "").replace(/[^\p{L}\p{N}]/gu, "")) {
        el.setAttribute("aria-label", title);
      }
    }
    return el.getAttribute("data-tip") ?? "";
  }

  function hide() {
    clearTimeout(timer);
    text = "";
    target = null;
  }

  function place(el: Element) {
    const r = el.getBoundingClientRect();
    below = r.bottom + 80 < window.innerHeight;
    x = r.left + r.width / 2;
    y = below ? r.bottom + 6 : r.top - 6;
    // Keep it on screen once it has a size.
    queueMicrotask(() => {
      if (!tipEl) return;
      const w = tipEl.offsetWidth;
      x = Math.min(Math.max(x, w / 2 + 8), window.innerWidth - w / 2 - 8);
    });
  }

  onMount(() => {
    const over = (e: PointerEvent) => {
      const el = (e.target as Element | null)?.closest?.("[title], [data-tip]");
      if (el === target) return;
      hide();
      if (!el || el.closest(".xterm")) return;
      const tip = tipOf(el);
      if (!tip) return;
      target = el;
      timer = setTimeout(() => {
        if (target !== el || !el.isConnected) return;
        text = tip;
        place(el);
      }, DELAY);
    };
    window.addEventListener("pointerover", over);
    window.addEventListener("pointerdown", hide, true);
    window.addEventListener("keydown", hide, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);
    return () => {
      window.removeEventListener("pointerover", over);
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
