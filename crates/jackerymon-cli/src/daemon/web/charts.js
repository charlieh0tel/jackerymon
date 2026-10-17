// Charts shared by the daemon's pages: colors, sizes, uPlot setup, and
// y axes whose ticks are round numbers on both sides.  Loaded after
// common.js, before the page's own script.

"use strict";

// The chart library comes from a CDN; without it the strip and the unit
// table still work, and the chart panel says why it is empty.
const haveUplot = typeof uPlot !== "undefined";
// Input and output in the TUI's colors; battery in the accent.
const INPUT = "#4ade80";
const AC_INPUT = "#a78bfa";
const CAR_INPUT = "#f472b6";
const SOLAR_INPUT = "#fb923c";
// The total input is a thin line over its sources, which should sum to it.
const TOTAL_WIDTH = 1;
const OUTPUT = "#fbbf24";
const BATTERY = "#38bdf8";
const TEMPERATURE = "#f87171";
const VOLTS = "#818cf8";
const HERTZ = "#2dd4bf";
// About this many points per chart, whatever the window.
const POINTS = 1500;
// How often a window ending now is read again.
const CHART_EVERY = 30000;
const CHART_HEIGHT = 180;
// Y axes have a fixed width, and a chart without a right axis reserves
// the same room on the right, so the charts' plot areas, and with them
// their time axes, line up.  An axis has no title: each number carries
// its unit, in the color of the line it measures.  Wide enough for
// "1000 W", "32.5 °C" and the state strip's "power out".
const AXIS_SIZE = 80;
const AXIS_DIM = "#8b929c";
// How many divisions a chart's y axes may have; fewer first, so a tie
// takes the roomier.
const DIVISIONS = [3, 4];
// Round steps between ticks, times a power of ten.
const STEP_MANTISSAS = [1, 2, 2.5, 5];
// The smallest steps worth a tick: whole percent, half a degree.
const PERCENT_STEP = 1;
const CELSIUS_STEP = 0.5;
// Volts come in tenths and hertz whole, but a step under 1 of either
// only magnifies noise.
const VOLT_STEP = 1;
const HERTZ_STEP = 1;
const FULL = 100;
// Hovering one chart moves the cursor on the others.
const SYNC = haveUplot ? uPlot.sync("jackerymon") : null;

// The charts drawn, in order, so they can be taken down.
let charts = [];

function clearCharts(why, bad = false) {
  for (const c of charts) c.destroy();
  charts = [];
  $("charts").innerHTML = "";
  $("note").textContent = bad ? why : "";
  if (!bad) $("charts").innerHTML = `<span class="muted">${esc(why)}</span>`;
}

function chart(title, data, series, axes, scales = {}) {
  const el = document.createElement("div");
  $("charts").appendChild(el);
  const right = axes.some((axis) => axis.side === 1);
  return new uPlot(
    {
      title,
      width: $("charts").clientWidth || 800,
      height: CHART_HEIGHT,
      // null leaves a side to uPlot, which pads nothing beside an axis.
      padding: [null, right ? null : AXIS_SIZE, null, null],
      // No horizontal crosshair: the charts' y scales differ, and uPlot
      // parks a synced chart's line just above its plot, where it shows.
      // No drag to zoom: the next read would redraw the charts and drop it.
      cursor: { y: false, drag: { x: false, y: false }, sync: { key: SYNC.key } },
      series: [{}, ...series],
      axes: [{ stroke: AXIS_DIM, grid: { stroke: "#2b3038" } }, ...axes],
      scales: { x: { time: true }, ...scales },
    },
    data,
    el,
  );
}

// A left y axis on the scale named for its unit, with its gridlines,
// its numbers in `stroke`, on the ticks of aligned fit `f` if given.
const yAxis = (scale, stroke = AXIS_DIM, f = null) => ({
  scale, size: AXIS_SIZE, stroke, grid: { stroke: "#2b3038" },
  ...unitValues(scale, f), ...(f ? alignedTicks(f) : {}),
});

// Decimals a tick needs at `step`: 0 for 5, 1 for 0.5, 2 for 0.25.
function stepDecimals(step) {
  let decimals = 0;
  while (Math.abs(step * 10 ** decimals - Math.round(step * 10 ** decimals)) > 1e-9) decimals++;
  return decimals;
}

// The smallest round step that is at least `least`.
function roundStep(least) {
  for (let power = Math.floor(Math.log10(least)); ; power++) {
    for (const mantissa of STEP_MANTISSAS) {
      const step = mantissa * 10 ** power;
      if (step >= least - 1e-9) return step;
    }
  }
}

// A range of `divisions` round steps covering [low, high], as near
// centered on it as the steps allow, and below `ceiling` when it can be.
function fit({ low, high, least, ceiling = Infinity }, divisions) {
  for (let step = roundStep(Math.max(least, (high - low) / divisions)); ; step = roundStep(step * 1.000001)) {
    const decimals = stepDecimals(step);
    const centered = (low + high) / 2 - (divisions * step) / 2;
    let min = Number((Math.floor(centered / step + 1e-9) * step).toFixed(decimals));
    if (min + divisions * step > ceiling && ceiling - divisions * step <= low) min = ceiling - divisions * step;
    const max = min + divisions * step;
    if (max >= high - 1e-9) return { min, max, step, decimals, divisions };
  }
}

// Ranges for a chart's left and right scales with the same number of
// divisions, so every gridline is a round number on both axes.  Each
// scale is { low, high } of its data, `least` its smallest step, and an
// optional `ceiling`.  A scale with no data is given [0, least].
function aligned(scales) {
  let best = null;
  for (const divisions of DIVISIONS) {
    const fits = scales.map((scale) => fit(scale, divisions));
    const waste = fits.reduce((sum, f, i) => sum + 1 - (scales[i].high - scales[i].low) / (f.max - f.min), 0);
    if (!best || waste < best.waste - 1e-9) best = { waste, fits };
  }
  return best.fits;
}

// The extent of a series, ignoring gaps.
function extent(values, least) {
  const present = values.filter((v) => v != null);
  return present.length
    ? { low: Math.min(...present), high: Math.max(...present), least }
    : { low: 0, high: least, least };
}

// The fixed scale and its ticks for one side of an aligned pair.
const alignedScale = (f) => ({ range: () => [f.min, f.max] });
const alignedTicks = (f) => ({
  splits: () => Array.from({ length: f.divisions + 1 }, (_, i) => Number((f.min + i * f.step).toFixed(f.decimals))),
});

// Tick numbers with their unit, to the decimals of an aligned fit, or
// as uPlot chose them, less floating-point noise.
const unitValues = (unit, f) => ({
  values: (u, ticks) => ticks.map((t) => `${f ? t.toFixed(f.decimals) : Number(t.toFixed(6))} ${unit}`),
});

// A right y axis, on ticks aligned with the left's, so it draws no
// gridlines of its own.
const rightAxis = (scale, stroke, f) => ({
  scale, side: 1, size: AXIS_SIZE, stroke, grid: { show: false },
  ...unitValues(scale, f), ...alignedTicks(f),
});
