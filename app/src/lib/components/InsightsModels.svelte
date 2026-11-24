<script lang="ts">
  import type { InsightsModels, ModelRow } from "#lib/api.ts";
  import { app } from "#lib/state.svelte.ts";

  // What models really cost: a merged PR's price, with the rework it took.
  let { data }: { data: InsightsModels } = $props();

  const SIZES: [ModelRow["size"], string][] = [
    ["small", "Small, up to 100 lines"], ["medium", "Medium, up to 500 lines"], ["large", "Large, over 500 lines"], ["unknown", "Size unknown"],
  ];
  const usd = (n: number) => (n >= 100 ? `$${Math.round(n).toLocaleString()}` : `$${n.toFixed(2)}`);
  const hours = (h: number | null) => (h == null ? "—" : h < 1 ? `${Math.round(h * 60)} min` : h < 48 ? `${h.toFixed(1)} h` : `${(h / 24).toFixed(1)} days`);
  const total = (r: ModelRow) => r.first + r.rework;
  const most = $derived(Math.max(...data.rows.map(total), 0.01));
  const left = $derived(Object.entries(data.left_out).sort((a, b) => b[1] - a[1]));
  const prs = (n: number) => `${n} PR${n === 1 ? "" : "s"}`;
</script>

{#if app.defaults && !app.defaults.workflow}
  <p class="muted">The ticket workflow is off, so no new PRs are followed. Turn it on with <code>workflow.enabled</code>.</p>
{/if}
{#if !data.merged}
  <div class="empty">No merged PRs in these weeks. The watcher follows the PRs of your sprint's tickets in <code>watch.github_repos</code>.</div>
{:else}
  <div class="row head">
    <h3 class="grow">Build cost per merged PR, with rework</h3>
    <span class="key"><i class="first"></i>First pass</span><span class="key"><i class="again"></i>Rework</span>
  </div>
  {#each SIZES as [size, name]}
    {@const rows = data.rows.filter((r) => r.size === size)}
    {#if rows.length}
      <section>
        <h4>{name}</h4>
        <div class="scrollx">
          <table>
            <thead><tr><th class="model">Model</th><th class="cost">Cost</th><th class="num">Per PR</th><th class="num">Of it rework</th><th class="num">Reworked</th><th class="num" title="Median, from ready for review">To merge</th><th class="num" title="Median heads that others reviewed">Review rounds</th></tr></thead>
            <tbody>
              {#each rows as r}
                <tr>
                  <th scope="row">{r.model}<div class="muted small">{r.reviewed ? "with a reviewer" : "no reviewer"}{r.enough ? "" : ` · only ${prs(r.prs)}, too few to compare`}</div></th>
                  <td class="cost"><div class="bar" title={`First pass ${usd(r.first)}, rework ${usd(r.rework)}`}>{#if r.first}<div class="first" style:width={`${(100 * r.first) / most}%`}></div>{/if}{#if r.rework}<div class="again" style:width={`${(100 * r.rework) / most}%`}></div>{/if}</div></td>
                  <td class="num" title={r.unpriced ? `${r.unpriced} runs reported no cost` : ""}>{usd(total(r))}{r.unpriced ? "+" : ""}</td>
                  <td class="num">{usd(r.rework)}</td>
                  <td class="num">{r.reworked} of {r.prs}</td>
                  <td class="num">{hours(r.hours)}</td>
                  <td class="num">{r.rounds ?? "—"}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </section>
    {/if}
  {/each}
  {#if !data.rows.length}<p class="muted">None of the {prs(data.merged)} merged can be compared yet.</p>{/if}
  {#if left.length}
    <section>
      <h4>Left out</h4>
      <ul>{#each left as [why, n]}<li>{prs(n)}: {why}</li>{/each}</ul>
    </section>
  {/if}
  <p class="muted small">A PR took rework if, once it was ready for review, it was pushed to again, its CI failed or changes were requested, or if its build was sent back. The builder's and its reviewer's runs from then on are the rework, counted against the model that built it. A builder's runs are shared evenly by its open PRs, so costs of PRs built together are estimates. A group needs 10 PRs to compare, and differences aren't proof of a cause.</p>
{/if}

<style>
  h3, h4 { margin: 0; font-size: 13px; }
  h4 { font-weight: 600; margin-bottom: 4px; }
  .head { margin-bottom: 14px; }
  .small { font-size: 11px; }
  section { margin-bottom: 22px; }
  .scrollx { overflow-x: auto; }
  table { width: 100%; min-width: 720px; table-layout: fixed; border-collapse: collapse; font-size: 12px; }
  th { text-align: left; font-weight: 400; padding: 6px 8px 6px 0; }
  thead th { color: var(--muted); }
  td { padding: 6px 8px; }
  tbody :is(th, td) { border-top: 1px solid var(--border); }
  thead th.num { padding-right: 8px; }
  th.model { width: 26%; }
  .cost { width: 24%; padding-left: 0; }
  .bar { display: flex; gap: 2px; height: 14px; }
  .bar div:last-child { border-radius: 0 4px 4px 0; }
  .num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .first { background: var(--chart-1); }
  .again { background: var(--chart-2); }
  .key { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; }
  .key i { width: 10px; height: 10px; border-radius: 2px; }
  ul { margin: 0; padding-left: 18px; }
  .empty { color: var(--muted); max-width: 70ch; }
</style>
