<script lang="ts">
  let { text, marked, class: klass = "" }: { text: string; marked?: number[]; class?: string } = $props();

  // Big diffs (a lockfile, generated code) have tens of thousands of lines:
  // only the ones in view, plus a margin, are in the page.
  const ROW = 18;
  let diffEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewH = $state(600);
  const diffLines = $derived(text.split("\n"));
  const mark = $derived(marked ? new Set(marked) : null);
  const widest = $derived(diffLines.reduce((m, l) => Math.max(m, l.length + 7 * (l.split("\t").length - 1)), 0));
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 30));
  const last = $derived(Math.min(diffLines.length, Math.ceil((scrollTop + viewH) / ROW) + 30));
  $effect(() => {
    text;
    if (diffEl) diffEl.scrollTop = scrollTop = 0;
  });

  function cls(line: string) {
    if (line.startsWith("+++") || line.startsWith("---")) return "meta";
    if (line.startsWith("+")) return "add";
    if (line.startsWith("-")) return "del";
    if (line.startsWith("@@")) return "hunk";
    if (line.startsWith("diff ")) return "file";
    return "";
  }

  function extra(line: string, i: number) {
    if (!mark) return "";
    if (mark.has(i)) return " marked";
    return cls(line) === "add" ? " dim" : "";
  }
</script>

<div class="scroll diff {klass}" bind:this={diffEl} bind:clientHeight={viewH} onscroll={() => diffEl && (scrollTop = diffEl.scrollTop)}>
  <div class="rows" style="height: {diffLines.length * ROW}px; width: calc({widest}ch + 28px)">
    <div style="transform: translateY({first * ROW}px)">
      {#each diffLines.slice(first, last) as l, i (first + i)}<div class="l {cls(l)}{extra(l, first + i)}">{l || " "}</div>{/each}
    </div>
  </div>
</div>

<style>
  .diff { font-family: var(--mono); font-size: 12px; padding: 8px 0; min-height: 0; }
  .rows { min-width: 100%; }
  .l { white-space: pre; padding: 0 14px; height: 18px; line-height: 18px; overflow: hidden; }
  .add { background: color-mix(in srgb, var(--green) 14%, transparent); }
  .del { background: color-mix(in srgb, var(--red) 14%, transparent); }
  .hunk { color: var(--cyan); }
  .file { color: var(--purple); font-weight: 600; border-top: 1px solid var(--border); }
  .meta { color: var(--muted); }
  .marked { box-shadow: inset 3px 0 0 var(--accent); }
  .dim { opacity: .5; }
</style>
