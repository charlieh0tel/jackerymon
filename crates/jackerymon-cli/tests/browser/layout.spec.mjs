// The page's layout with a scrollbar that takes room, as a desktop
// browser's does.  Headless Chromium hides scrollbars, and turning that
// off is a launch option, so these tests have a file of their own.

import { expect, test } from "@playwright/test";

import { Fake } from "./fake.mjs";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });

test("the charts' time axes line up when drawing them adds a scrollbar", async ({ page }) => {
  const fake = new Fake();
  // Tall enough that the page scrolls only once the charts are drawn.
  await page.setViewportSize({ width: 1000, height: 800 });
  await fake.install(page);
  await page.goto("/");
  await expect(page.locator("#charts .uplot")).toHaveCount(3);
  const plots = page.locator("#charts .u-over");
  const boxes = () =>
    plots.evaluateAll((all) => all.map((p) => p.getBoundingClientRect()).map(({ left, width }) => ({ left, width })));
  await expect.poll(async () => {
    const [first, ...rest] = await boxes();
    return rest.every(({ left, width }) => left === first.left && width === first.width);
  }).toBe(true);
  // And they fill the panel as it is now, not as it was.
  const [power] = await boxes();
  const panel = await page.locator("#charts").evaluate((e) => e.clientWidth);
  expect(power.width).toBeLessThan(panel);
});

test("hovering a chart does not move the page", async ({ page }) => {
  const fake = new Fake();
  fake.root = "overview.html";
  const add = (serial, name, model, model_name) =>
    fake.units.push({ serial, name, model, model_name, link: { state: "connected" } });
  add("855225011707296", "bob", 14, "Explorer 3000");
  add("860125092302121", "darryl", 19, "HomePower 3000");
  add("855124090905001", "mikko", 12, "Explorer 2000 v2");
  // A width at which hover values once wrapped the legend onto a new line.
  await page.setViewportSize({ width: 900, height: 900 });
  await fake.install(page);
  await page.goto("/?last=21600");
  await expect(page.locator("#charts .uplot")).toHaveCount(7);
  const heights = () =>
    page.locator("#charts .uplot").evaluateAll((all) => all.map((chart) => chart.getBoundingClientRect().height));
  const before = await heights();
  const plot = await page.locator("#charts .u-over").nth(1).boundingBox();
  await page.mouse.move(plot.x + plot.width / 2, plot.y + plot.height / 2);
  await expect(page.locator("#charts .u-legend").nth(1)).not.toContainText("--");
  expect(await heights()).toEqual(before);
});
