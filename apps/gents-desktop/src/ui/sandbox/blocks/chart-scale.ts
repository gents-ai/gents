/* The geometry behind the SDK's charts: pure functions from rows and a size
   to positions and labels, so what a chart draws can be checked without a
   layout engine. */
import { formatNumber, numeric, timeOf } from "./format";

export type ChartRow = Record<string, unknown>;

/* the frame defines eight categorical colors; a ninth series has none */
export const MAX_SERIES = 8;

export const seriesColor = (index: number) => `var(--chart-${index + 1})`;

export function seriesKeys(y: string | string[]): string[] {
  return (Array.isArray(y) ? y : [y]).slice(0, MAX_SERIES);
}

export interface Plot {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

export interface GridLine {
  y: number;
  label: string;
}

export interface XTick {
  x: number;
  label: string;
  anchor: "start" | "middle" | "end";
}

export interface Point {
  x: number;
  y: number;
  value: number;
}

const TICK_CHAR_PX = 6.2;
const LABEL_GAP_PX = 12;
const MAX_LABEL_CHARS = 18;

function step(rough: number): number {
  const magnitude = 10 ** Math.floor(Math.log10(rough));
  const normal = rough / magnitude;
  const nice =
    normal <= 1 ? 1 : normal <= 2 ? 2 : normal <= 2.5 ? 2.5 : normal <= 5 ? 5 : 10;
  return nice * magnitude;
}

/** round tick values that cover `lo..hi` */
export function niceTicks(lo: number, hi: number, count = 4): number[] {
  const low = lo === hi ? (lo === 0 ? 0 : Math.min(lo, 0)) : lo;
  const high = lo === hi ? (hi === 0 ? 1 : Math.max(hi, 0)) : hi;
  const size = step((high - low) / count);
  const first = Math.floor(low / size);
  const last = Math.ceil(high / size);
  const ticks: number[] = [];
  for (let i = first; i <= last; i++) ticks.push(Number((i * size).toPrecision(12)));
  return ticks;
}

/* a line may leave zero out when the values sit far above it (latency around
   250ms would otherwise be a flat line at the top); a bar's length is its
   value, so bars always start at zero */
export function valueTicks(values: number[], zeroBased: boolean): number[] {
  let lo = Math.min(...values);
  let hi = Math.max(...values);
  if (zeroBased || (lo >= 0 && lo <= hi * 0.5) || (hi <= 0 && hi >= lo * 0.5)) {
    lo = Math.min(lo, 0);
    hi = Math.max(hi, 0);
  }
  return niceTicks(lo, hi);
}

const clip = (label: string) =>
  label.length > MAX_LABEL_CHARS ? `${label.slice(0, MAX_LABEL_CHARS - 1)}…` : label;

/* the y labels' longest width sets the left margin, so a long figure is
   never cut off at the edge */
function plotFor(width: number, height: number, tickLabels: string[]): Plot {
  const longest = Math.max(...tickLabels.map((label) => label.length), 1);
  return {
    left: Math.ceil(longest * TICK_CHAR_PX + 8),
    right: width - 12,
    top: 8,
    bottom: height - 22,
  };
}

function gridFor(ticks: number[], plot: Plot) {
  const lo = ticks[0]!;
  const hi = ticks[ticks.length - 1]!;
  const yOf = (value: number) =>
    plot.bottom - ((value - lo) / (hi - lo)) * (plot.bottom - plot.top);
  const grid: GridLine[] = ticks.map((tick) => ({
    y: yOf(tick),
    label: formatNumber(tick),
  }));
  return { yOf, grid };
}

function anchorAt(x: number, plot: Plot): XTick["anchor"] {
  const edge = (plot.right - plot.left) * 0.08;
  if (x < plot.left + edge) return "start";
  if (x > plot.right - edge) return "end";
  return "middle";
}

/** how many labels of this length fit along the plot without touching */
function labelsThatFit(labels: string[], plot: Plot): number {
  const longest = Math.max(...labels.map((label) => label.length), 1);
  const slot = longest * TICK_CHAR_PX + LABEL_GAP_PX;
  return Math.max(1, Math.floor((plot.right - plot.left) / slot));
}

/* every k-th category, so the labels never collide; the tooltip names the
   ones left out */
function ordinalTicks(labels: string[], xOf: (i: number) => number, plot: Plot) {
  const clipped = labels.map(clip);
  const every = Math.ceil(clipped.length / labelsThatFit(clipped, plot));
  const ticks: XTick[] = [];
  for (let i = 0; i < clipped.length; i += every) {
    ticks.push({ x: xOf(i), label: clipped[i]!, anchor: anchorAt(xOf(i), plot) });
  }
  return ticks;
}

const DAY_MS = 86_400_000;
const timeFormat = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
});
const dateFormat = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
});

/* hours only when the points are hours apart; daily points a day apart are
   still days */
function timeFormatter(times: number[]) {
  const span = times[times.length - 1]! - times[0]!;
  const closest = times
    .slice(1)
    .reduce((gap, time, i) => Math.min(gap, time - times[i]!), Infinity);
  const format = span < 2 * DAY_MS && closest < DAY_MS ? timeFormat : dateFormat;
  return (time: number) => format.format(new Date(time));
}

function categoryLabel(value: unknown): string {
  if (value === null || value === undefined) return "—";
  const time = timeOf(value);
  return time === null ? String(value) : dateFormat.format(new Date(time));
}

export interface LineModel {
  plot: Plot;
  grid: GridLine[];
  ticks: XTick[];
  /** per series, a point per row; null where the row has no number */
  series: Array<Array<Point | null>>;
  /** the x of each row, for the hover layer */
  xs: number[];
  /** the rows as drawn: by time when the x field holds dates */
  rows: ChartRow[];
  /** a label per row, for the tooltip and the table view */
  labels: string[];
}

export function lineModel(
  data: ChartRow[],
  x: string,
  keys: string[],
  width: number,
  height: number,
): LineModel | null {
  const values = data.flatMap((row) =>
    keys.map((key) => numeric(row[key])).filter((v): v is number => v !== null),
  );
  if (values.length === 0) return null;
  const times = data.map((row) => timeOf(row[x]));
  const timed = times.every((time) => time !== null);
  const order = data.map((_, i) => i);
  if (timed) order.sort((a, b) => times[a]! - times[b]!);
  const rows = order.map((i) => data[i]!);
  const ticks = valueTicks(values, false);
  const plot = plotFor(width, height, ticks.map(formatNumber));
  const { yOf, grid } = gridFor(ticks, plot);
  const inner = plot.right - plot.left;
  let xOf: (i: number) => number;
  let xTicks: XTick[];
  let labels: string[];
  if (timed) {
    const sortedTimes = order.map((i) => times[i]!);
    const start = sortedTimes[0]!;
    const span = sortedTimes[sortedTimes.length - 1]! - start;
    xOf = (i) =>
      span === 0
        ? (plot.left + plot.right) / 2
        : plot.left + ((sortedTimes[i]! - start) / span) * inner;
    const label = timeFormatter(sortedTimes);
    labels = sortedTimes.map(label);
    const fit = labelsThatFit(labels, plot);
    if (rows.length <= fit) {
      xTicks = labels.map((label, i) => ({
        x: xOf(i),
        label,
        anchor: anchorAt(xOf(i), plot),
      }));
    } else {
      /* more points than labels: even steps in time, not in rows, so a burst
         of points does not crowd the axis */
      xTicks = Array.from({ length: fit }, (_, i) => {
        const time = start + (span * i) / Math.max(fit - 1, 1);
        const px = plot.left + (span === 0 ? 0 : ((time - start) / span) * inner);
        return { x: px, label: label(time), anchor: anchorAt(px, plot) };
      });
    }
  } else {
    const gap = rows.length > 1 ? inner / (rows.length - 1) : 0;
    xOf = (i) => (rows.length > 1 ? plot.left + gap * i : (plot.left + plot.right) / 2);
    labels = rows.map((row) => categoryLabel(row[x]));
    xTicks = ordinalTicks(labels, xOf, plot);
  }
  const series = keys.map((key) =>
    rows.map((row, i) => {
      const value = numeric(row[key]);
      return value === null ? null : { x: xOf(i), y: yOf(value), value };
    }),
  );
  return {
    plot,
    grid,
    ticks: xTicks,
    series,
    xs: rows.map((_, i) => xOf(i)),
    rows,
    labels,
  };
}

/** the line through a series, broken across rows with no number */
export function linePath(points: Array<Point | null>): string {
  let path = "";
  let open = false;
  for (const point of points) {
    if (!point) {
      open = false;
      continue;
    }
    path += `${open ? "L" : "M"}${point.x} ${point.y}`;
    open = true;
  }
  return path;
}

/** the last point of a series, where a line is direct-labelled */
export function lastPoint(points: Array<Point | null>): Point | null {
  for (let i = points.length - 1; i >= 0; i--) {
    if (points[i]) return points[i]!;
  }
  return null;
}

const MAX_BAR_PX = 24;
const BAR_GAP_PX = 2;

export interface Bar {
  series: number;
  x: number;
  width: number;
  /** the y of the bar's far end */
  y: number;
  value: number;
}

export interface BarModel {
  plot: Plot;
  grid: GridLine[];
  ticks: XTick[];
  baseline: number;
  bars: Bar[][];
  /** each category's band, for the hover layer */
  bands: Array<{ x: number; width: number }>;
  labels: string[];
}

export function barModel(
  data: ChartRow[],
  x: string,
  keys: string[],
  width: number,
  height: number,
): BarModel | null {
  const values = data.flatMap((row) =>
    keys.map((key) => numeric(row[key])).filter((v): v is number => v !== null),
  );
  if (values.length === 0) return null;
  const ticks = valueTicks(values, true);
  const plot = plotFor(width, height, ticks.map(formatNumber));
  const { yOf, grid } = gridFor(ticks, plot);
  const baseline = yOf(0);
  const band = (plot.right - plot.left) / data.length;
  const room = Math.max(band * 0.7, 2);
  const barWidth = Math.max(
    Math.min(MAX_BAR_PX, (room - BAR_GAP_PX * (keys.length - 1)) / keys.length),
    1,
  );
  const groupWidth = barWidth * keys.length + BAR_GAP_PX * (keys.length - 1);
  const bars = data.map((row, i) => {
    const center = plot.left + band * (i + 0.5);
    return keys.flatMap((key, s) => {
      const value = numeric(row[key]);
      if (value === null) return [];
      return [
        {
          series: s,
          x: center - groupWidth / 2 + s * (barWidth + BAR_GAP_PX),
          width: barWidth,
          y: yOf(value),
          value,
        },
      ];
    });
  });
  const labels = data.map((row) => categoryLabel(row[x]));
  return {
    plot,
    grid,
    ticks: ordinalTicks(labels, (i) => plot.left + band * (i + 0.5), plot).map((t) => ({
      ...t,
      anchor: "middle" as const,
    })),
    baseline,
    bars,
    bands: data.map((_, i) => ({ x: plot.left + band * i, width: band })),
    labels,
  };
}

/** a bar with a rounded far end and a square foot on the baseline */
export function barPath(bar: Bar, baseline: number): string {
  const { x, width, y } = bar;
  const right = x + width;
  const up = y < baseline;
  const r = Math.min(4, width / 2, Math.abs(baseline - y));
  const inner = up ? y + r : y - r;
  return [
    `M${x} ${baseline}`,
    `V${inner}`,
    `Q${x} ${y} ${x + r} ${y}`,
    `H${right - r}`,
    `Q${right} ${y} ${right} ${inner}`,
    `V${baseline}`,
    "Z",
  ].join("");
}

export interface Slice {
  label: string;
  value: number;
  share: number;
  /** a series color; the fold of the smallest slices is drawn neutral */
  color: string;
}

const MAX_SLICES = 7;

/* a slice keeps the color of its place in the data, not of its size, so a
   shift in the numbers does not repaint the chart */
export function donutSlices(data: ChartRow[], label: string, value: string): Slice[] {
  const entries = data.flatMap((row, index) => {
    const amount = numeric(row[value]);
    if (amount === null || amount <= 0) return [];
    return [{ label: categoryLabel(row[label]), value: amount, index }];
  });
  const total = entries.reduce((sum, entry) => sum + entry.value, 0);
  const keep =
    entries.length <= MAX_SLICES
      ? new Set(entries.map((entry) => entry.index))
      : new Set(
          [...entries]
            .sort((a, b) => b.value - a.value)
            .slice(0, MAX_SLICES - 1)
            .map((entry) => entry.index),
        );
  const slices: Slice[] = entries
    .filter((entry) => keep.has(entry.index))
    .map((entry, position) => ({
      label: entry.label,
      value: entry.value,
      share: entry.value / total,
      color: seriesColor(position),
    }));
  const rest = entries.filter((entry) => !keep.has(entry.index));
  if (rest.length > 0) {
    const amount = rest.reduce((sum, entry) => sum + entry.value, 0);
    slices.push({
      label: "Other",
      value: amount,
      share: amount / total,
      color: "var(--text-muted)",
    });
  }
  return slices;
}

/** one ring segment, from `start` to `end` radians clockwise from the top */
export function arcPath(
  center: number,
  outer: number,
  inner: number,
  start: number,
  end: number,
): string {
  /* a full turn has no distinct end point for an SVG arc to reach */
  const stop = Math.min(end, start + Math.PI * 2 - 0.0001);
  const large = stop - start > Math.PI ? 1 : 0;
  const at = (radius: number, angle: number) =>
    `${center + radius * Math.sin(angle)} ${center - radius * Math.cos(angle)}`;
  return [
    `M${at(outer, start)}`,
    `A${outer} ${outer} 0 ${large} 1 ${at(outer, stop)}`,
    `L${at(inner, stop)}`,
    `A${inner} ${inner} 0 ${large} 0 ${at(inner, start)}`,
    "Z",
  ].join("");
}
