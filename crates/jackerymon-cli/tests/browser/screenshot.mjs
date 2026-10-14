// The README's screenshot of the page, from a running daemon.
//
//   node screenshot.mjs URL LAST OUT
//
// URL is the daemon's TCP address, LAST the chart window in seconds,
// OUT the PNG to write.

import { chromium } from "@playwright/test";

const [url, last, out] = process.argv.slice(2);
// Wide enough for the charts' legends on one line.
const VIEWPORT = { width: 1100, height: 900 };

const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: VIEWPORT, colorScheme: "dark" });
  await page.goto(`${url}/?last=${last}`);
  await page.locator("#charts .uplot").nth(1).waitFor();
  await page.locator("#status .pill").waitFor();
  await page.screenshot({ path: out, fullPage: true });
} finally {
  await browser.close();
}
