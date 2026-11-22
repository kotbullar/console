// The Event Log through the bridge (LOG_READ, LINK-42): every record read, newest first, dated from its age,
// filtered by source, a position linking to the map, and the CSV export.
import { readFileSync } from "node:fs";
import { connect, disconnect, expect, onRadio, openConsole, shot, state, test } from "./helpers";

test.afterEach(async ({ page }) => disconnect(page));

const events = (page: import("@playwright/test").Page) => page.getByRole("table", { name: "Events" }).locator("tbody tr[data-seq]");

test("event log: the radio's records, newest first, by source, to CSV", async ({ page }, info) => {
  await page.route(/tile\.openstreetmap\.org/, (r) => r.abort());
  await openConsole(page, "activity");
  await connect(page);
  await expect.poll(() => state(page, (f) => f.activity.rows.length as number), { timeout: 20_000 }).toBeGreaterThan(0);
  const rows = await state(page, (f) => f.activity.rows as { seq: number; at: number; ago_s: number; source: string; lat: number | null }[]);
  const held = await state(page, (f) => f.activity.held as number);
  expect(rows.length).toBe(held);
  for (let i = 1; i < rows.length; i++) expect(rows[i - 1].seq).toBeGreaterThan(rows[i].seq);
  // each dated from its age at the read: within a few seconds of now - age
  const now = Date.now();
  for (const r of rows) expect(Math.abs(now - r.ago_s * 1000 - r.at)).toBeLessThan(60_000);
  await expect(events(page)).toHaveCount(rows.length);
  await shot(page, info, "activity-01-all");

  if (!onRadio()) {
    // the simulator's sample log (ferrite-app's `sample.rs`): APRS, pager and SAME events
    const sources = new Set(rows.map((r) => r.source));
    for (const s of ["APRS", "Pager", "SAME"]) expect(sources).toContain(s);
    await page.getByLabel("Source").selectOption("Pager");
    const pager = rows.filter((r) => r.source === "Pager").length;
    await expect(events(page)).toHaveCount(pager);
    await expect(events(page).first()).toContainText("capcode");
    await shot(page, info, "activity-02-pager");
    await page.getByLabel("Source").selectOption("all");
  }

  // CSV: a header and one line a row shown
  const [dl] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export CSV" }).click()]);
  expect(dl.suggestedFilename()).toMatch(/^ferrite-.*-event-log\.csv$/);
  const csv = readFileSync(await dl.path(), "utf8").trim().split(/\r?\n/);
  expect(csv[0]).toMatch(/^time,seq,source,slot,frequency_mhz,name,text/);
  expect(csv.length).toBe(rows.length + 1);

  // a position goes to the map
  const withPos = rows.find((r) => r.lat !== null);
  if (withPos) {
    await page.locator(`tr[data-seq="${withPos.seq}"] a`).click();
    await expect(page).toHaveURL(/#\/aprs-map\?at=/);
  }
});

test("activation contacts travel from Tools to the CSV", async ({ page }) => {
  test.skip(onRadio(), "uses a simulated activation");
  await openConsole(page, "live");
  await connect(page);
  await page.getByRole("button", { name: "More Live View options" }).click();
  await page.getByRole("button", { name: "Send a key sequence…" }).click();
  const box = page.getByRole("combobox", { name: "Key sequence" });
  await box.fill("MENU 0 3 8 MENU 4 4 4 8 1 # 0 0 0 1 MENU 2 2 # 1 # 2 MENU");
  await box.press("Enter");
  await expect.poll(() => state(page, (f) => f.radio.ui?.screen?.id), { timeout: 20_000 }).toBe("Activation");
  await page.evaluate(() => { location.hash = "/activity"; });
  await page.getByLabel("Source").selectOption("activations");
  await expect(events(page)).toHaveCount(1);
  await expect(events(page).first()).toContainText("A1A");
  const [file] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export CSV" }).click()]);
  expect(file.suggestedFilename()).toMatch(/^ferrite-.*-activations\.csv$/);
  const csv = readFileSync(await file.path(), "utf8");
  expect(csv).toContain("time_utc,program,reference,callsign,frequency_mhz,mode");
  expect(csv).toContain('"POTA","IT-0001","A1A"');
});
