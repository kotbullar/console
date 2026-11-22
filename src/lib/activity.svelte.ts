// The Event Log's state: the radio's event log (LOG_READ, LINK-42) read whole, newest first, each record dated
// from its age at the read (the radio's clock may have no date; a time it doesn't know stays unknown, INBOX-6.1);
// then, from a radio that has them (HELLO caps 128), each new event as it is logged (SUBSCRIBE's mask 8, LINK-57)
// while the Event Log or the Live View's panel is open. Filters and the CSV are the page's and the core's.
import { atOf } from "./aprs.svelte";
import { logCsv, P, type AprsEvent, type LogRecord } from "./core";
import { download, stamp } from "./channels.svelte";
import { RadioError } from "./link";
import { radio } from "./radio.svelte";

export type LogRow = LogRecord & { at: number | null };

class Activity {
  rows = $state<LogRow[]>([]);
  held = $state(0);
  busy = $state("");
  error = $state("");
  readAt = $state(0);
  /** Events that arrived live since the read (LINK-57). */
  live = $state(0);
  private forLink: unknown = null;
  private users = 0;
  private release: (() => void) | null = null;

  /** The radio sends its new events as they happen (LINK-57). */
  get canLive() {
    return !!radio.connectedLink;
  }

  /** A page wants new events as they happen (the Event Log, the Live View's panel): subscribed while any does. */
  use(): () => void {
    if (this.users++ === 0) {
      const stopWant = radio.want("log", 8);
      const stopEvents = radio.onEvents((ev, gap) => this.onEvent(ev, gap));
      this.release = () => {
        stopWant();
        stopEvents();
      };
    }
    return () => {
      if (--this.users === 0) {
        this.release?.();
        this.release = null;
      }
    };
  }

  private onEvent(ev: AprsEvent | null, gap: boolean) {
    // Events lost on the way: the log read again (the radio's count tells what is missing).
    if (gap) {
      if (this.readAt && !this.busy) void this.refresh();
      return;
    }
    if (ev?.kind !== "log") return;
    const r = ev.record;
    if (this.rows.some((x) => x.seq === r.seq)) return;
    // LINK-57: events the log dropped before they could be sent are skipped; read the gap.
    const newest = this.rows[0]?.seq;
    if (this.readAt && newest !== undefined && r.seq > newest + 1 && !this.busy) {
      void this.refresh();
      return;
    }
    this.rows = [{ ...r, at: atOf(r.ago_s, Date.now()) }, ...this.rows];
    this.held++;
    this.live++;
  }

  ensure() {
    const link = radio.connectedLink;
    if (link && this.forLink !== link && !this.busy) void this.refresh();
  }

  async refresh() {
    const link = radio.connectedLink;
    if (!link) return;
    this.forLink = link;
    this.error = "";
    this.busy = "Reading the event log";
    try {
      const now = Date.now();
      const p = await link.log((n) => (this.busy = `Reading the event log: ${n}`));
      this.rows = p.records.map((r) => ({ ...r, at: atOf(r.ago_s, now) }));
      this.held = p.held;
      this.readAt = now;
      this.live = 0;
    } catch (e) {
      this.error = e instanceof RadioError && e.status === 2 ? "This radio's firmware has no event log over the link yet (LINK-42): update it." : (e as Error).message;
      this.forLink = null;
    } finally {
      this.busy = "";
    }
  }

  async activationCsv(rows: LogRow[]) {
    this.error = "";
    const fields = rows.map((r) => [r.at === null ? "" : new Date(r.at).toISOString(), r.source, r.text, r.name, r.mhz, r.detail]);
    const csv = ["time_utc,program,reference,callsign,frequency_mhz,mode",
      ...fields.map((row) => row.map((value) => `"${value.replaceAll('"', '""')}"`).join(","))].join("\r\n") + "\r\n";
    try {
      await download(`ferrite-${stamp()}-activations.csv`, csv, "text/csv");
    } catch (e) {
      this.error = (e as Error).message;
    }
  }

  /** The rows shown (`rows`, filtered) as a CSV download, dated in local time. */
  csv(rows: LogRow[]) {
    const times = rows.map((r) => (r.at === null ? "" : new Date(r.at).toLocaleString())); // INBOX-6.1: unknown left blank
    const strip = rows.map(({ at: _at, ...r }) => r);
    const who = (radio.info?.callsign || "radio").replace(/[^A-Za-z0-9-]/g, "");
    void download(`ferrite-${who}-${stamp()}-event-log.csv`, logCsv(strip, times), "text/csv").catch((e) => console.error("event log CSV:", e));
  }
}

export const activity = new Activity();
