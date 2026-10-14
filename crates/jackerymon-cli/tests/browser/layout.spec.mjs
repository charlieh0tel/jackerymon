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
