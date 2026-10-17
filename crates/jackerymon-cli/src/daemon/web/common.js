// What the page shares with any later one: fetching that never hangs,
// the status strip, a chooser of the daemon's units, the range
// control, and the lifecycle that decides when content reads and what it
// does when the daemon goes away.  The page is served at a unit's own
// address, `/units/<serial>/`, and reads that unit's data relative to
// it.  After
// smartclock-web's common.js, so the fleet's pages behave alike at the
// edges.  Loaded as a classic script before the page's own, which ends
// by calling `content` once.

const $ = (id) => document.getElementById(id);
// Everything interpolated into innerHTML goes through this.
const esc = (v) =>
  String(v).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const MISSING = "--";
const fmt = (v, digits = 0) =>
  v === null || v === undefined ? MISSING : Number(v).toFixed(digits);
const unitText = (v, unit, digits = 0) =>
  v === null || v === undefined ? MISSING : `${fmt(v, digits)} ${unit}`;

// How long a read may take, how often the strip polls, and how old a
// reading may be before it shows as stale (the daemon's own /metrics
// cut-off).
const QUICK_TIMEOUT = 5000;
const STRIP_EVERY = 1000;
const STALE_AFTER = 60;

// ------------------------------------------------------------- fetching

// Total: never rejects, never hangs, so a failure is shown in place
// rather than stopping the timers.
async function getJson(url) {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(QUICK_TIMEOUT) });
    const body = await response.json();
    if (!response.ok) return { error: body?.error ?? `the daemon answered ${response.status}` };
    return body;
  } catch (e) {
    return { error: String(e) };
  }
}

// An answer for a range nobody is looking at any more; dropped quietly.
class Superseded extends Error {}

// Read JSON for the page's content, or throw.  `signal` is aborted when
// the page moves on, and anything arriving after that throws Superseded
// rather than being drawn.
async function ask(url, signal, timeout = QUICK_TIMEOUT) {
  let body;
  try {
    const r = await fetch(url, { signal: AbortSignal.any([signal, AbortSignal.timeout(timeout)]) });
    body = await r.json();
    if (!r.ok) throw new Error(body?.error ?? `the daemon answered ${r.status}`);
  } catch (e) {
    if (signal.aborted) throw new Superseded();
    if (e.name === "TimeoutError") throw new Error(`no answer within ${timeout / 1000} s`);
    throw e;
  }
  if (signal.aborted) throw new Superseded();
  return body;
}

// ---------------------------------------------------------------- strip

// The last /status the strip read, for the page's own panels.
let latest = null;
let ticking = false;

async function tick() {
  if (ticking) return;
  ticking = true;
  try {
    const s = await getJson("status");
    if (s.error) {
      latest = null;
      $("status").innerHTML =
        `<span class="pill down">NO DAEMON</span> <span class="muted">${esc(s.error)}</span>`;
    } else {
      latest = s;
      render(s);
    }
    page.status?.(latest);
  } finally {
    ticking = false;
  }
}

// Seconds since a sample was taken, by its own timestamp; null for none.
const ageAt = (taken) => (taken ? Math.max(0, (Date.now() - Date.parse(taken)) / 1000) : null);

// A unit's state as a pill, its class and its word, from its link and
// the age of its latest reading.
function linkPill(link, age) {
  return link?.state === "lost" ? ["down", "RECONNECTING"]
    : link?.state === "failed" ? ["down", "RESTARTING"]
    : age === null ? ["stale", "SEARCHING"]
    : age > STALE_AFTER ? ["stale", "STALE"]
    : ["live", "LIVE"];
}

function render(s) {
  const v = s.view ?? {};
  const age = ageAt(s.latest?.timestamp);
  const state = linkPill(s.link, age);
  const stats = [
    ["battery", unitText(v.battery_percent, "%")],
    ["input", unitText(v.input_watts, "W")],
    ["output", unitText(v.output_watts, "W")],
    ["battery temp", unitText(v.battery_celsius, "°C", 1)],
    ["state", v.battery_state ?? MISSING],
    ["to full", unitText(v.hours_to_full, "h", 1)],
    ["to empty", unitText(v.hours_to_empty, "h", 1)],
  ];
  $("status").innerHTML =
    `<span class="pill ${state[0]}">${state[1]}</span>` +
    `<span id="unit-name">${esc(s.name ?? productName(s))}</span>` +
    `<span class="muted" id="age">${age === null ? "no reading yet" : `updated ${age.toFixed(0)} s ago`}</span>` +
    stats
      .map(([k, value]) => `<span class="stat"><span class="v">${esc(value)}</span><span class="k">${esc(k)}</span></span>`)
      .join("") +
    (v.fault && v.fault !== "none" ? `<span class="pill down">fault ${esc(v.fault)}</span>` : "") +
    (v.temperature_alarm && v.temperature_alarm !== "none"
      ? `<span class="pill stale">temperature ${esc(v.temperature_alarm)}</span>`
      : "") +
    (s.link?.error ? ` <span class="muted">${esc(s.link.error)}</span>` : "");
  document.title = `Jackery ${unitLabel(s)}`;
  colophon(`Jackery ${unitLabel(s)}`, s);
}

// The footer: what the page shows, then the daemon's version, author,
// license and links from a unit's /status `s`.
function colophon(what, s) {
  const link = (href, text) => `<a href="${esc(href)}">${esc(text)}</a>`;
  $("colophon").innerHTML = [
    esc(what),
    `jackerymon ${esc(s.version ?? "")}`,
    "read-only",
    `© ${esc(s.author ?? "")}`,
    esc(s.license ?? ""),
    link(s.repository, "source"),
    link(`${s.repository}/issues`, "issues"),
  ].join(" · ");
}

// ---------------------------------------------------------------- units

// The product's name, or its model code when the daemon has no name.
const productName = (u) => u.model_name ?? `model ${u.model}`;
// A unit's name from the daemon's names file, if any, its product and its
// serial.
const unitLabel = (u) => [u.name, productName(u), u.serial].filter(Boolean).join(" ");

// The units the daemon serves, connected ones first, from `/units`, two levels up
// from a unit's page; empty until read.  Read again this often, so each
// one's link state stays current.
let units = [];
const UNITS_EVERY = 5000;
// A link state as the chooser words it.
const LINK_WORDS = { connected: "connected", searching: "searching", lost: "reconnecting", failed: "restarting" };

async function readUnits() {
  const listed = await getJson("../../units");
  units = Array.isArray(listed) ? listed : [];
  drawUnits();
}

// A chooser of units with their link states, and a link to the
// overview of them all, hidden for one unit.  Choosing one opens its
// page with the same chart window.
function drawUnits() {
  $("units").hidden = units.length < 2;
  $("all-units").href = `../../${location.search}`;
  const select = $("unit-choice");
  const label = (u) => `${unitLabel(u)}: ${LINK_WORDS[u.link?.state] ?? MISSING}`;
  const same =
    select.options.length === units.length && units.every((u, i) => select.options[i].value === u.serial);
  // Relabeled in place when the units are the same, so an open list stays open.
  if (same) units.forEach((u, i) => (select.options[i].textContent = label(u)));
  else select.innerHTML = units.map((u) => `<option value="${esc(u.serial)}">${esc(label(u))}</option>`).join("");
  if (latest?.serial) select.value = latest.serial;
  select.onchange = () => {
    location.href = `../${encodeURIComponent(select.value)}/${location.search}`;
  };
}

// ------------------------------------------------------------ lifecycle

// The page, as it described itself to `content`: `load(signal)` draws
// its content, `clear(why, bad)` takes it down saying why, `every` and
// `auto()` say when to read again, `status(s)` sees each strip read, and
// `live`, pairs of a read and its period in milliseconds, keeps the
// live parts current; a unit page's strip and chooser when absent.
let page = null;
// The read in progress, and whether another was asked for meanwhile.
let running = null;
let again = false;

function refresh() {
  if (running) {
    again = true;
    return;
  }
  const control = new AbortController();
  running = control;
  page
    .load(control.signal)
    .catch((e) => {
      if (!(e instanceof Superseded)) page.clear(e.message, true);
    })
    .finally(() => {
      if (running !== control) return;
      running = null;
      if (again) {
        again = false;
        refresh();
      }
    });
}

// Drop whatever is being read and start over, as when the range changes.
function renew() {
  running?.abort();
  running = null;
  again = false;
  refresh();
}

async function content(spec) {
  page = spec;
  const live = page.live ?? [
    [tick, STRIP_EVERY],
    [readUnits, UNITS_EVERY],
  ];
  for (const [read, every] of live) setInterval(read, every);
  if (page.every) {
    setInterval(() => {
      if (page.auto()) refresh();
    }, page.every);
  }
  refresh();
  // In order the first time: the chooser marks the unit the strip read.
  for (const [read] of live) await read();
}

// ---------------------------------------------------------------- range

// The chart windows offered, in seconds, and the default; any other
// length can be typed in a unit below.
const RANGE_PRESETS = [["10 min", 600], ["6 h", 21600], ["24 h", 86400], ["7 d", 604800]];
const DEFAULT_RANGE = 21600;
const RANGE_UNITS = [["min", 60], ["h", 3600], ["d", 86400], ["w", 604800]];

// The window shown, kept in the address so a reload or a shared link
// keeps it.  Any whole number of minutes.
function readRange() {
  const last = Number(new URLSearchParams(location.search).get("last"));
  return Number.isInteger(last / 60) && last > 0 ? last : DEFAULT_RANGE;
}

// A length as a count of the biggest unit that divides it.
function splitLength(seconds) {
  const [, size] = [...RANGE_UNITS].reverse().find(([, size]) => Number.isInteger(seconds / size));
  return [seconds / size, size];
}

// Presets, and "last [N] [unit]" for any other length.
function rangeControl(el, current, changed) {
  const [count, size] = splitLength(current);
  el.innerHTML =
    RANGE_PRESETS
      .map(([label, secs]) => `<button data-last="${secs}" aria-pressed="${secs === current}">${esc(label)}</button>`)
      .join(" ") +
    ` <label>last <input id="range-count" type="number" min="1" step="1" value="${count}">` +
    ` <select id="range-unit">` +
    RANGE_UNITS.map(([name, s]) => `<option value="${s}"${s === size ? " selected" : ""}>${name}</option>`).join("") +
    `</select></label>`;
  const choose = (last) => {
    const q = new URLSearchParams(location.search);
    q.set("last", String(last));
    history.replaceState(null, "", `${location.pathname}?${q}`);
    rangeControl(el, last, changed);
    changed(last);
  };
  for (const b of el.querySelectorAll("button")) b.onclick = () => choose(Number(b.dataset.last));
  const typed = () => {
    const n = Number($("range-count").value);
    // Anything but a whole number above zero puts the box back.
    if (Number.isInteger(n) && n > 0) choose(n * Number($("range-unit").value));
    else rangeControl(el, current, changed);
  };
  $("range-count").onchange = typed;
  $("range-unit").onchange = typed;
}
