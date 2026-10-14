// The daemon's page against a faked API: what the strip says in each
// state of the daemon and its link, the charts and their ranges, and
// what is left when the chart library or the history is missing.

import { expect, test } from "@playwright/test";

import { Fake } from "./fake.mjs";

async function open(page, fake, path = "/") {
  await fake.install(page);
  await page.goto(path);
}

test("the strip shows the latest reading as live", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  const strip = page.locator("#status");
  await expect(strip.locator(".pill")).toHaveText("LIVE");
  await expect(strip).toContainText("87 %");
  await expect(strip).toContainText("412 W");
  await expect(strip).toContainText("24.1 °C");
  await expect(strip).toContainText("charging");
  const colophon = page.locator("#colophon");
  await expect(colophon).toContainText("Jackery 855125071405677");
  await expect(colophon).toContainText("© Christopher Hoover");
  await expect(colophon.locator("a", { hasText: "issues" })).toHaveAttribute(
    "href",
    "https://github.com/charlieh0tel/jackerymon/issues",
  );
});

test("an old reading is stale, a lost link says why", async ({ page }) => {
  const fake = new Fake();
  fake.age = 300;
  await open(page, fake);
  await expect(page.locator("#status .pill").first()).toHaveText("STALE");
  fake.lost = "device disconnected";
  await expect(page.locator("#status .pill").first()).toHaveText("RECONNECTING");
  await expect(page.locator("#status")).toContainText("device disconnected");
});

test("the strip says when the daemon goes and recovers when it returns", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  await expect(page.locator("#status .pill")).toHaveText("LIVE");
  fake.up = false;
  await expect(page.locator("#status .pill")).toHaveText("NO DAEMON");
  await expect(page.locator("#unit")).toContainText("no daemon");
  fake.up = true;
  await expect(page.locator("#status .pill")).toHaveText("LIVE");
});

test("no reading yet is said, not shown as zeros", async ({ page }) => {
  const fake = new Fake();
  fake.age = null;
  await open(page, fake);
  await expect(page.locator("#status .pill")).toHaveText("SEARCHING");
  await expect(page.locator("#status")).toContainText("no reading yet");
  await expect(page.locator("#unit")).toContainText("no reading yet");
});

test("the unit table lists what the unit reports, read-only", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  const unit = page.locator("#unit");
  await expect(unit).toContainText("on, 120.9 V 60 Hz");
  await expect(unit).toContainText("12 h");
  // USB and car outputs are not reported by this model, so not listed.
  await expect(unit).not.toContainText("USB output");
  await expect(page.locator("#unit button, #unit input, #unit select")).toHaveCount(0);
});

test("two charts are drawn from /series for the default range", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  await expect(page.locator("#charts")).toContainText("Power");
  await expect(page.locator("#charts")).toContainText("Battery");
  const q = fake.query.series;
  const span = (Date.parse(q.get("until")) - Date.parse(q.get("since"))) / 1000;
  expect(span).toBe(21600);
  await expect(page.locator('#ranges button[aria-pressed="true"]')).toHaveText("6 h");
});

test("the charts' time axes line up", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  const [power, battery] = await page
    .locator("#charts .u-over")
    .evaluateAll((plots) => plots.map((p) => p.getBoundingClientRect()).map(({ left, width }) => ({ left, width })));
  expect(battery).toEqual(power);
});

test("a range button reads that range and keeps it in the address", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  await page.locator("#ranges button", { hasText: "7 d" }).click();
  await expect(page).toHaveURL(/last=604800/);
  await expect
    .poll(() => {
      const q = fake.query.series;
      return (Date.parse(q.get("until")) - Date.parse(q.get("since"))) / 1000;
    })
    .toBe(604800);
  await page.reload();
  await expect(page.locator('#ranges button[aria-pressed="true"]')).toHaveText("7 d");
});

test("a read left behind is neither drawn nor reported as a failure", async ({ page }) => {
  const fake = new Fake();
  fake.delay.series = 1500;
  await open(page, fake);
  // Leaves the default range's slow read behind; this one is slow too.
  await page.locator("#ranges button", { hasText: "10 min" }).click();
  // Before the new answer arrives, the abandoned read must not show as
  // an error.  Read once: a retrying assertion would wait for the next
  // answer to clear it.
  await page.waitForTimeout(300);
  expect(await page.locator("#note").textContent()).toBe("");
  // Leave this one behind too, for a fast range with nothing in it.
  fake.series = { ...fake.series, time: [] };
  fake.delay.series = 0;
  await page.locator("#ranges button", { hasText: "24 h" }).click();
  await expect(page.locator("#charts")).toContainText("no samples logged in this window");
  // The abandoned reads would have drawn charts; none appear.
  await page.waitForTimeout(2000);
  await expect(page.locator("#charts .uplot")).toHaveCount(0);
  await expect(page.locator("#note")).toHaveText("");
});

test("a daemon without a log says so in the chart panel", async ({ page }) => {
  const fake = new Fake();
  fake.series = null;
  await open(page, fake);
  await expect(page.locator("#note")).toContainText("no --log-dir");
  await expect(page.locator("#status .pill")).toHaveText("LIVE");
});

test("without the chart library the strip still works", async ({ page }) => {
  const fake = new Fake();
  await fake.install(page);
  await page.route("https://cdn.jsdelivr.net/**", (route) => route.abort());
  await page.goto("/");
  await expect(page.locator("#note")).toContainText("could not be loaded");
  await expect(page.locator("#status .pill")).toHaveText("LIVE");
});
