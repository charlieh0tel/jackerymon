// The page's files from the source tree and a fake of the daemon's API,
// installed on a Playwright page.  A test decides whether the daemon is
// up, whether its link is lost, how old its reading is, and how long an
// answer takes: the edges the page must handle, which a live daemon
// cannot be made to show on demand.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = (name) => JSON.parse(readFileSync(join(here, "fixtures", `${name}.json`), "utf8"));
const RECORDED = { status: fixture("status"), series: fixture("series") };
const PAGES = join(here, "..", "..", "src", "daemon", "web");
const FILES = {
  "/": ["index.html", "text/html; charset=utf-8"],
  "/style.css": ["style.css", "text/css; charset=utf-8"],
  "/common.js": ["common.js", "text/javascript; charset=utf-8"],
};
const UPLOT = join(here, "node_modules", "uplot", "dist");

// The daemon's state, as a test drives it.
//   up        whether the daemon answers at all
//   lost      the link error, or null while connected
//   failed    why polling ended, or null; it starts again shortly
//   units     the /units answer: the units the daemon serves
//   age       seconds since the latest sample, or null for none yet
//   series    the /series answer, or null for 404 (no --log-dir)
//   delay     milliseconds to hold each endpoint's answer, by name
export class Fake {
  constructor() {
    this.up = true;
    this.lost = null;
    this.failed = null;
    const { serial, model, model_name } = RECORDED.status;
    this.units = [{ serial, model, model_name, link: { state: "connected" } }];
    this.age = 2;
    this.series = structuredClone(RECORDED.series);
    this.delay = {};
    // Requests seen, by endpoint, and the last query of each.
    this.seen = {};
    this.query = {};
  }

  status() {
    const body = structuredClone(RECORDED.status);
    if (this.lost) body.link = { state: "lost", error: this.lost, consecutive: 1 };
    if (this.failed) body.link = { state: "failed", error: this.failed };
    if (this.age === null) {
      body.latest = null;
      body.view = null;
    } else {
      body.latest.timestamp = new Date(Date.now() - this.age * 1000).toISOString();
    }
    return body;
  }

  answer(endpoint) {
    if (endpoint === "status") return [200, this.status()];
    if (endpoint === "units") return [200, this.units];
    if (endpoint === "series") {
      return this.series ? [200, this.series] : [404, { error: "no history: the daemon has no --log-dir" }];
    }
    return [404, { error: "not found" }];
  }

  async install(page) {
    await page.route("https://cdn.jsdelivr.net/npm/uplot@1.6.32/dist/*", (route) =>
      route.fulfill({ path: join(UPLOT, new URL(route.request().url()).pathname.split("/").pop()) }),
    );
    await page.route("http://jackerymon.test/**", async (route) => {
      const url = new URL(route.request().url());
      // The daemon serves a unit's page and data under its own address,
      // and the list of units at /units.
      const path = url.pathname.replace(/^\/units\/[^/]+(?=\/)/, "");
      const file = FILES[path];
      if (file) {
        await route.fulfill({ path: join(PAGES, file[0]), contentType: file[1] });
        return;
      }
      const endpoint = path.slice(1);
      this.seen[endpoint] = (this.seen[endpoint] ?? 0) + 1;
      this.query[endpoint] = url.searchParams;
      if (!this.up) {
        await route.abort("connectionrefused").catch(() => {});
        return;
      }
      // The answer as of the request, delivered after any delay, as a
      // daemon that read then stalled would.
      const [status, body] = this.answer(endpoint);
      const wait = this.delay[endpoint] ?? 0;
      if (wait) await new Promise((r) => setTimeout(r, wait));
      // The page may have gone on without this answer.
      await route
        .fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
        .catch(() => {});
    });
  }
}
