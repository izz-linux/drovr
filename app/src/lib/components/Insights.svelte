<script lang="ts">
  import { api, type InsightsModels, type Insights, type InsightsPart, type InsightsTotal, type InsightsWeek } from "#lib/api.ts";

  import { app } from "#lib/state.svelte.ts";
  import Retros from "./Retros.svelte";
  import Models from "./InsightsModels.svelte";

  // What runs cost, week by week, from the spend log, and how PRs turned out.
  let tab = $state<"overview" | "models" | "retros">("overview");
  let span = $state(8);
  let data = $state<Insights | null>(null);
  let prs = $state<InsightsModels | null>(null);
  let error = $state("");
  let asTable = $state(false);
  let shown = $state<number | null>(null);

  /** The Mondays that start the last `n` weeks, and the next one, in seconds. */
  function bounds(n: number): number[] {
    const now = new Date();
    const monday = now.getDate() - ((now.getDay() + 6) % 7);
    return Array.from({ length: n + 1 }, (_, i) =>
      new Date(now.getFullYear(), now.getMonth(), monday + 7 * (i - n + 1)).getTime() / 1000);
  }

  $effect(() => {
    const n = span;
    let gone = false;
    const load = () => {
      const b = bounds(n);
      Promise.all([api.insights(b), api.insightsModels(b[0], b[n])])
        .then(([d, m]) => { if (!gone) { data = d; prs = m; error = ""; } })
        .catch((e) => { if (!gone) error = String(e); });
    };
    shown = null;
    load();
    const t = setInterval(load, 30_000);
    return () => { gone = true; clearInterval(t); };
  });

  // A harness keeps its color whatever else is on the chart.
  const HARNESSES = ["claude", "opencode", "codex"];
  const NAMES: Record<string, string> = { claude: "Claude Code", opencode: "OpenCode", codex: "Codex", other: "Other" };
  const series = [...HARNESSES, "other"];
  const color = (s: string) => `var(--chart-${series.indexOf(s) + 1})`;
  const cost = (w: InsightsWeek, s: string) =>
    w.by_harness.filter((p) => (HARNESSES.includes(p.name) ? p.name : "other") === s).reduce((a, p) => a + p.cost, 0);
  const present = $derived(series.filter((s) => data?.weeks.some((w) => cost(w, s) > 0)));

  const usd = (n: number) => (n >= 100 ? `$${Math.round(n).toLocaleString()}` : `$${n.toFixed(2)}`);
  const hours = (h: number) => (h < 1 ? `${Math.round(h * 60)} min` : h < 48 ? `${h.toFixed(1)} h` : `${(h / 24).toFixed(1)} days`);
  const day = (ts: number) => new Date(ts * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const runs = (n: number) => `${n.toLocaleString()} run${n === 1 ? "" : "s"}`;
  const unknown = (t: InsightsTotal) => (t.unpriced ? `${runs(t.unpriced)} with no cost reported` : "");
  const last = $derived((data?.weeks.length ?? 0) - 1);
  const label = (w: InsightsWeek, i: number) =>
    [`Week of ${day(w.start)}${i === last ? ", so far" : ""}: ${usd(w.cost)}, ${runs(w.runs)}`,
      ...present.filter((s) => cost(w, s) > 0).map((s) => `${NAMES[s]} ${usd(cost(w, s))}`),
      unknown(w), w.retros.length ? `retro ${w.retros.map(day).join(", ")}` : ""].filter(Boolean).join("; ");

  // The axis ends on a round number, at or above everything drawn.
  const top = $derived.by(() => {
    const most = Math.max(data?.weekly_target_usd ?? 0, ...(data?.weeks.map((w) => w.cost) ?? []), 1);
    const unit = 10 ** Math.floor(Math.log10(most));
    return unit * ([1, 2, 4, 6, 8, 10].find((m) => m * unit >= most) ?? 10);
  });
  const parts = $derived<[string, InsightsPart[]][]>(
    data ? [["Harness", data.by_harness], ["Model", data.by_model], ["Stage", data.by_stage], ["Ticket", data.by_ticket]] : []);
  const ROWS = 8;
  let all = $state<Record<string, boolean>>({});
</script>

{#snippet breakdown(title: string, list: InsightsPart[])}
  {@const most = Math.max(...list.map((p) => p.cost), 0.01)}
  <section>
    <h3>By {title.toLowerCase()}</h3>
    <table>
      <thead class="hidden"><tr><th>{title}</th><th>Share</th><th>Cost</th><th>Runs</th></tr></thead>
      <tbody>
        {#each all[title] ? list : list.slice(0, ROWS) as p (p.name)}
          <tr title={[p.name, unknown(p)].filter(Boolean).join(": ")}>
            <th scope="row">{title === "Harness" ? (NAMES[p.name] ?? p.name) : p.name}</th>
            <td class="track"><div class="bar" style:width={`${(100 * p.cost) / most}%`}></div></td>
            <td class="num">{p.runs && p.runs === p.unpriced ? "not reported" : `${usd(p.cost)}${p.unpriced ? "+" : ""}`}</td>
            <td class="num muted">{p.runs ? runs(p.runs) : ""}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if list.length > ROWS}<button class="ghost" onclick={() => (all[title] = !all[title])}>{all[title] ? "Show fewer" : `Show all ${list.length}`}</button>{/if}
  </section>
{/snippet}

<div class="wrap">
  <header class="row">
    <h2>Insights</h2>
    <div class="row grow tabs">
      {#each [["overview", "Overview"], ["models", "Models"], ["retros", "Retros"]] as const as [id, name]}<button class="ghost" class:on={tab === id} aria-pressed={tab === id} onclick={() => (tab = id)}>{name}</button>{/each}
    </div>
    <button class="ghost" hidden={tab !== "overview"} aria-pressed={asTable} onclick={() => (asTable = !asTable)}>{asTable ? "Show chart" : "Show table"}</button>
    <select hidden={tab === "retros"} aria-label="Weeks shown" bind:value={span}>
      {#each [4, 8, 12, 26] as n}<option value={n}>Last {n} weeks</option>{/each}
    </select>
  </header>
  {#if error}<div class="banner err">{error}</div>{/if}

  <div class="scroll grow">
    {#if tab === "retros"}
      {#if app.defaults && !app.defaults.workflow}<p class="muted">The ticket workflow is off, and retros are part of it. Turn it on with <code>workflow.enabled</code>.</p>{:else}<Retros />{/if}
    {:else if tab === "models"}
      {#if prs}<Models data={prs} />{/if}
    {:else if data && !data.total.runs}
      <div class="empty">No runs in these weeks. drovr logs what each run costs as it ends.</div>
    {:else if data}
      <div class="tiles">
        <div class="tile"><span class="muted">Spent</span><b>{usd(data.total.cost)}</b><span class="muted small">{runs(data.total.runs)}</span></div>
        <div class="tile"><span class="muted">Average week</span><b>{data.week_average == null ? "—" : usd(data.week_average)}</b><span class="muted small">{data.weekly_target_usd ? `target ${usd(data.weekly_target_usd)}` : "full weeks only"}</span></div>
        <div class="tile"><span class="muted">Average retro</span><b>{data.retro_average == null ? "—" : usd(data.retro_average)}</b><span class="muted small">{data.retro_average == null ? "none logged" : "the retro agent's runs"}</span></div>
        {#if prs?.merged}
          <div class="tile"><span class="muted">Per merged PR</span><b>{usd(data.total.cost / prs.merged)}</b><span class="muted small">everything spent ÷ {prs.merged} merged</span></div>
          <div class="tile"><span class="muted">PRs reworked</span><b>{prs.whole ? `${Math.round((100 * prs.reworked) / prs.whole)}%` : "—"}</b><span class="muted small">{prs.whole ? `${prs.reworked} of ${prs.whole} seen from the start` : "none seen from the start"}</span></div>
          <div class="tile"><span class="muted">Ready to merged</span><b>{prs.hours == null ? "—" : hours(prs.hours)}</b><span class="muted small">median</span></div>
        {/if}
        {#if data.total.unpriced}<div class="tile"><span class="muted">Cost not reported</span><b>{data.total.unpriced.toLocaleString()}</b><span class="muted small">runs the totals leave out</span></div>{/if}
      </div>

      <section>
        <div class="row">
          <h3 class="grow">Spend by week</h3>
          {#each present as s}<span class="key"><i style:background={color(s)}></i>{NAMES[s]}</span>{/each}
          <span class="key"><i class="flag">◆</i>Retro</span>
        </div>
        {#if asTable}
          <table class="weeks">
            <thead><tr><th>Week of</th>{#each present as s}<th class="num">{NAMES[s]}</th>{/each}<th class="num">Total</th><th class="num">Runs</th><th class="num">Not reported</th><th>Retros</th></tr></thead>
            <tbody>
              {#each data.weeks as w, i}
                <tr><th scope="row">{day(w.start)}{i === last ? " (so far)" : ""}</th>{#each present as s}<td class="num">{usd(cost(w, s))}</td>{/each}<td class="num">{usd(w.cost)}</td><td class="num">{w.runs.toLocaleString()}</td><td class="num">{w.unpriced || ""}</td><td>{w.retros.map(day).join(", ")}</td></tr>
              {/each}
            </tbody>
          </table>
        {:else}
          <div class="chart">
            <div class="axis muted small"><span>{usd(top)}</span><span>{usd(top / 2)}</span><span>$0</span></div>
            <div class="plot">
              <div class="line" style:bottom="100%"></div><div class="line" style:bottom="50%"></div>
              {#if data.weekly_target_usd}<div class="line target" style:bottom={`${(100 * data.weekly_target_usd) / top}%`}><span class="small">target</span></div>{/if}
              {#each data.weeks as w, i}
                <button class="week" class:now={i === last} aria-label={label(w, i)}
                  onmouseenter={() => (shown = i)} onmouseleave={() => (shown = null)} onfocus={() => (shown = i)} onblur={() => (shown = null)}>
                  <div class="stack" style:height={`${(100 * w.cost) / top}%`}>
                    {#each present as s}{#if cost(w, s) > 0}<div class="seg" style:flex-grow={cost(w, s)} style:background={color(s)}></div>{/if}{/each}
                  </div>
                </button>
              {/each}
              {#if shown !== null && data.weeks[shown]}
                {@const w = data.weeks[shown]}
                <div class="tip" class:left={shown > last / 2} style:--at={`${(100 * (shown + 0.5)) / data.weeks.length}%`}>
                  <b>Week of {day(w.start)}{shown === last ? ", so far" : ""}</b>
                  <div>{usd(w.cost)} · {runs(w.runs)}</div>
                  {#each present as s}{#if cost(w, s) > 0}<div class="key"><i style:background={color(s)}></i>{NAMES[s]} {usd(cost(w, s))}</div>{/if}{/each}
                  {#if w.unpriced}<div class="muted">{unknown(w)}</div>{/if}
                  {#if w.retros.length}<div>◆ Retro {w.retros.map(day).join(", ")}</div>{/if}
                </div>
              {/if}
            </div>
            <div class="names muted small" aria-hidden="true">
              {#each data.weeks as w, i}<span>{#if w.retros.length}<i class="flag">◆</i>{/if}{span > 12 && i % 4 ? "" : day(w.start)}</span>{/each}
            </div>
          </div>
        {/if}
      </section>

      <div class="cols">{#each parts as [title, list]}{@render breakdown(title, list)}{/each}</div>
      <p class="muted small">Costs are what each CLI reports. A + means some of its runs reported none. (OC) is a model run through OpenCode. A run on several tickets is split evenly between them.</p>
    {/if}
  </div>
</div>

<style>
  .wrap { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  header { padding: 10px 16px; border-bottom: 1px solid var(--border); gap: 12px; }
  h2 { margin: 0; font-size: 16px; }
  .tabs { gap: 2px; }
  .tabs .on { background: var(--panel-2); border-color: var(--border); }
  h3 { margin: 0 0 8px; font-size: 13px; }
  select { width: auto; }
  .small { font-size: 11px; }
  .err { color: var(--red); }
  .banner { padding: 6px 16px; font-size: 12px; border-bottom: 1px solid var(--border); white-space: pre-wrap; }
  .scroll { padding: 16px; }
  section { margin-bottom: 24px; min-width: 0; }
  .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; margin-bottom: 24px; }
  .tile { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 10px 12px; display: flex; flex-direction: column; }
  .tile b { font-size: 22px; font-weight: 600; }
  .key { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; }
  .key i:not(.flag) { width: 10px; height: 10px; border-radius: 2px; }
  .flag { font-style: normal; color: var(--text); font-size: 10px; }
  .chart { display: grid; grid-template-columns: auto 1fr; column-gap: 8px; margin-top: 14px; }
  .axis { display: flex; flex-direction: column; justify-content: space-between; text-align: right; height: 216px; margin: -8px 0; }
  .plot { position: relative; height: 200px; display: flex; border-bottom: 1px solid var(--muted); }
  .line { position: absolute; left: 0; right: 0; border-top: 1px solid var(--border); pointer-events: none; }
  .line.target { border-top: 2px solid var(--text); z-index: 1; }
  .line.target span { position: absolute; right: 0; bottom: 2px; background: var(--bg); padding: 0 4px; }
  .week { flex: 1 1 0; min-width: 0; height: 100%; padding: 0; border: none; border-radius: 0; background: transparent; display: flex; align-items: flex-end; justify-content: center; position: relative; cursor: default; }
  .week:hover { background: color-mix(in srgb, var(--text) 6%, transparent); }
  .stack { width: min(24px, 70%); display: flex; flex-direction: column-reverse; gap: 2px; }
  .seg { flex-basis: 0; min-height: 1px; }
  .seg:last-child { border-radius: 4px 4px 0 0; }
  .now .stack { opacity: .6; }
  .tip { position: absolute; top: 0; left: calc(var(--at) + 18px); z-index: 2; background: var(--panel-2); border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; font-size: 12px; white-space: nowrap; pointer-events: none; }
  .tip.left { left: auto; right: calc(100% - var(--at) + 18px); }
  .tip .key { display: flex; }
  .names { grid-column: 2; display: flex; padding-top: 4px; }
  .names span { flex: 1 1 0; min-width: 0; text-align: center; white-space: nowrap; display: flex; flex-direction: column; align-items: center; justify-content: flex-end; }
  .cols { display: grid; grid-template-columns: repeat(auto-fit, minmax(340px, 1fr)); gap: 0 32px; }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th { font-weight: 400; text-align: left; }
  .hidden { position: absolute; clip-path: inset(50%); width: 1px; height: 1px; overflow: hidden; }
  .cols th { max-width: 150px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 3px 8px 3px 0; }
  .track { width: 45%; padding: 3px 8px; }
  .bar { height: 10px; background: var(--chart-1); border-radius: 0 4px 4px 0; min-width: 1px; }
  .num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; padding-left: 8px; }
  .weeks th, .weeks td { padding: 5px 8px; border-top: 1px solid var(--border); }
  .weeks thead th { color: var(--muted); border-top: none; }
</style>
