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

test("the power chart estimates battery power as input minus output", async ({ page }) => {
  const fake = new Fake();
  const { input_watts: input, output_watts: output } = fake.series;
  [input[0], output[0]] = [300, 120];
  [input[1], output[1]] = [null, 50];
  await open(page, fake);
  await expect(page.locator("#charts")).toContainText("battery W, estimated");
  // `charts` is the page's own list of uPlot instances.
  const net = await page.evaluate(() => {
    const power = charts[0];
    const at = power.series.findIndex((series) => series.label?.startsWith("battery W"));
    return power.data[at].slice(0, 2);
  });
  expect(net).toEqual([180, null]);
});

// The Power chart's line labels, in order.
const powerLabels = (page) => page.evaluate(() => charts[0].series.slice(1).map((series) => series.label));

test("input sources that delivered nothing in the window get no line", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  expect(await powerLabels(page)).toEqual([
    "AC input W",
    "input W",
    "output W",
    "battery W, estimated (in − out)",
  ]);
});

test("each input source that delivered something gets its own line", async ({ page }) => {
  const fake = new Fake();
  fake.series.solar_input_watts = fake.series.solar_input_watts.map((v, i) => (i === 3 ? 120 : v));
  fake.series.car_input_watts = fake.series.car_input_watts.map(() => null);
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  const labels = await powerLabels(page);
  expect(labels).toContain("solar input W");
  expect(labels).not.toContain("car input W");
});

// A series fixture with the AC output on for the second half of the
// window, at a steady 60 Hz and a drifting voltage.
function acOutputOn(fake) {
  const n = fake.series.time.length;
  fake.series.ac_output_volts = fake.series.time.map((_, i) => (i < n / 2 ? null : 120 + (i % 5) / 2));
  fake.series.ac_output_hertz = fake.series.time.map((_, i) => (i < n / 2 ? null : 60));
}

test("the AC output chart appears only when the output was on in the window", async ({ page }) => {
  const fake = new Fake();
  fake.series.ac_output_volts = fake.series.time.map(() => null);
  fake.series.ac_output_hertz = fake.series.time.map(() => null);
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  acOutputOn(fake);
  await page.reload();
  await expect(page.locator("#charts .uplot")).toHaveCount(3);
  await expect(page.locator("#charts")).toContainText("AC output");
});

test("the AC output chart's hertz ticks fall on its voltage gridlines, and it lines up", async ({ page }) => {
  const fake = new Fake();
  acOutputOn(fake);
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(3);
  const { left, right, hz } = await page.evaluate(() => {
    const u = charts[2];
    const at = (axis) => u.axes[axis]._splits.map((v) => Math.round(u.valToPos(v, u.axes[axis].scale)));
    return { left: at(1), right: at(2), hz: u.scales.Hz };
  });
  expect(left.length).toBeGreaterThan(2);
  expect(right).toEqual(left);
  // A steady 60 Hz still gets a range around it.
  expect(hz.min).toBeLessThan(60);
  expect(hz.max).toBeGreaterThan(60);
  const boxes = await page
    .locator("#charts .u-over")
    .evaluateAll((plots) => plots.map((p) => p.getBoundingClientRect()).map(({ left, width }) => ({ left, width })));
  expect(boxes[2]).toEqual(boxes[0]);
});

// Where each tick of the battery chart's two y axes falls, in pixels,
// and the scale ranges, from the page's own uPlot instance.
const batteryTicks = (page) =>
  page.evaluate(() => {
    const u = charts[1];
    const at = (axis) => u.axes[axis]._splits.map((v) => Math.round(u.valToPos(v, u.axes[axis].scale)));
    return { left: at(1), right: at(2), percent: u.scales["%"], celsius: u.scales["°C"] };
  });

test("hovering a chart draws one vertical line on each chart and no horizontal one", async ({ page }) => {
  const fake = new Fake();
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  await page.locator("#charts .u-over").first().hover();
  await expect(page.locator("#charts .u-cursor-x")).toHaveCount(2);
  await expect(page.locator("#charts .u-cursor-y")).toHaveCount(0);
});

test("the battery chart's right axis ticks fall on the left axis gridlines", async ({ page }) => {
  const fake = new Fake();
  fake.series.battery_percent = fake.series.battery_percent.map((_, i) => 99 + (i % 2));
  fake.series.battery_celsius = fake.series.battery_celsius.map((_, i) => 27.4 + i / 10);
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  const { left, right, percent, celsius } = await batteryTicks(page);
  expect(left.length).toBeGreaterThan(2);
  expect(right).toEqual(left);
  // The ranges hold the data, and charge stays at or under 100 %.
  expect(percent.min).toBeLessThanOrEqual(99);
  expect(percent.max).toBe(100);
  expect(celsius.min).toBeLessThanOrEqual(27.4);
  expect(celsius.max).toBeGreaterThanOrEqual(27.4 + 5.9);
});

test("flat or missing battery data still gets ticks on both axes", async ({ page }) => {
  const fake = new Fake();
  fake.series.battery_percent = fake.series.battery_percent.map(() => 100);
  fake.series.battery_celsius = fake.series.battery_celsius.map(() => null);
  await open(page, fake);
  await expect(page.locator("#charts .uplot")).toHaveCount(2);
  const { left, right, percent } = await batteryTicks(page);
  expect(right).toEqual(left);
  expect(percent.max).toBe(100);
  expect(percent.min).toBeLessThan(100);
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
