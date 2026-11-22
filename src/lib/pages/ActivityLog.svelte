<script lang="ts">
  // Event Log: what the radio's event log holds (INBOX-6: APRS, pager, SAME, DSC, AIS, DTMF, Selcall, GPS),
  // newest first, read over the link (LOG_READ, LINK-42) when the page opens; then each new one as the radio logs
  // it (LINK-57, a radio that has it). Times are the PC's: the radio gives each record's age. Reading marks nothing
  // read on the radio.
  import { activity, type LogRow } from "../activity.svelte";
  import { ago, when } from "../aprs.svelte";
  import { radio } from "../radio.svelte";
  import { mapHref } from "../route.svelte";

  let source = $state("all");
  let search = $state("");
  let now = $state(Date.now());

  const connected = $derived(radio.phase === "connected");
  $effect(() => activity.use());
  $effect(() => {
    if (connected) activity.ensure();
  });
  const live = $derived((radio.subMask & 8) !== 0);
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 15_000);
    return () => clearInterval(t);
  });

  const sources = $derived([...new Set(activity.rows.map((r) => r.source))].sort());
  const rows = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return activity.rows.filter(
      (r: LogRow) => (source === "all" || r.source === source || (source === "activations" && (r.source === "SOTA" || r.source === "POTA"))) && (!q || `${r.name} ${r.text} ${r.detail} ${r.mhz}`.toLowerCase().includes(q)),
    );
  });
  const activationFilter = $derived(["activations", "SOTA", "POTA"].includes(source));
  const hasPos = (r: LogRow) => r.lat !== null && r.lon !== null;
</script>

<div class="pg">
  <div class="pg-bar" role="toolbar" aria-label="Event Log">
    <div class="grp">
      <button class="btn" disabled={!connected || !!activity.busy} onclick={() => activity.refresh()}>Refresh</button>
      <button class="btn" disabled={!rows.length} onclick={() => activationFilter ? activity.activationCsv(rows) : activity.csv(rows)} title="The rows shown, as a CSV file">Export CSV</button>
    </div>
    <div class="grp">
      <label class="label" for="log-source">Source</label>
      <select id="log-source" aria-label="Source" bind:value={source}>
        <option value="all">All</option>
        <option value="activations">SOTA / POTA</option>
        {#each sources as s (s)}<option value={s}>{s}</option>{/each}
      </select>
      <input type="search" placeholder="Find" aria-label="Find in the log" bind:value={search} />
    </div>
  </div>

  <div class="pg-status" aria-live="polite" data-status>
    {#if activity.busy}
      <span>{activity.busy}…</span>
    {:else if activity.readAt}
      <span>
        {activity.rows.length} of {activity.held} records, read {ago(activity.readAt, now)}{activity.live ? ` · ${activity.live} new since` : ""}{rows.length !== activity.rows.length ? ` · ${rows.length} shown` : ""}{live ? " · live" : ""}.
        Times are this computer's, from each record's age.
      </span>
    {:else if connected}
      <span>Reading…</span>
    {:else}
      <span>Connect to read the radio's event log.</span>
    {/if}
  </div>
  {#if activity.error}<div class="pg-banner">{activity.error}</div>{/if}

  {#if activity.readAt || activity.rows.length}
    <div class="pg-scroll">
      <table class="tbl" aria-label="Events">
        <thead>
          <tr>
            <th>Time</th>
            <th>Source</th>
            <th>Slot</th>
            <th>Frequency</th>
            <th>Name / call</th>
            <th>Text</th>
            <th>Position</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.seq)}
            <tr data-seq={r.seq}>
              <td class="nowrap" title={r.at === null ? "Stored before a restart, without GPS time: the radio doesn't know when" : ago(r.at, now)}>
                {r.at === null ? "Before restart" : when(r.at)}
                {#if r.unread}<span class="tag blue" title="Unread on the radio">new</span>{/if}
              </td>
              <td class="nowrap"><span class="tag">{r.source}</span></td>
              <td>{r.slot_name}</td>
              <td class="num">{r.mhz}</td>
              <td class="nowrap">{r.name}</td>
              <td>
                {r.text}
                {#if r.detail}<div class="dim">{r.detail}</div>{/if}
              </td>
              <td class="nowrap">
                {#if hasPos(r)}
                  <a href={mapHref({ lat: r.lat, lon: r.lon })} title="On the map">{r.lat!.toFixed(4)}, {r.lon!.toFixed(4)}</a>
                {/if}
              </td>
            </tr>
          {:else}
            <tr><td colspan="7" class="empty">{activity.rows.length ? "No record matches." : "The log is empty."}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  a {
    color: var(--blue);
  }
</style>
