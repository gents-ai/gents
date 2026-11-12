/* Line, bar and donut charts as plain SVG in the frame's theme tokens, so
   they repaint with the theme and pull in no charting library. Ink follows
   the data-viz rules the frame's palette was validated against: hairline
   grid, text in text tokens, a surface-colored ring or gap between marks. */
import { useCallback, useMemo, useState, type ReactNode } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@gents/ui/components/card";

import {
  arcPath,
  barModel,
  barPath,
  donutSlices,
  lastPoint,
  linePath,
  lineModel,
  seriesColor,
  seriesKeys,
  type ChartRow,
  type GridLine,
  type Plot,
  type XTick,
} from "./chart-scale";
import { formatNumber, humanize, numeric } from "./format";
import { EmptyState } from "./states";

const SURFACE = "var(--card)";
const DEFAULT_HEIGHT = 220;
const UNMEASURED_WIDTH = 480;

function useWidth() {
  const [width, setWidth] = useState(UNMEASURED_WIDTH);
  const ref = useCallback((element: HTMLDivElement | null) => {
    if (!element) return;
    const measure = () => {
      if (element.clientWidth > 0) setWidth(element.clientWidth);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width] as const;
}

function ChartCard({
  title,
  empty,
  children,
}: {
  title?: ReactNode;
  empty: boolean;
  children: ReactNode;
}) {
  return (
    <Card size="sm" data-slot="chart">
      {title && (
        <CardHeader>
          <CardTitle>{title}</CardTitle>
        </CardHeader>
      )}
      <CardContent>{empty ? <EmptyState title="No data" /> : children}</CardContent>
    </Card>
  );
}

function Legend({ keys }: { keys: string[] }) {
  if (keys.length < 2) return null;
  return (
    <ul className="mb-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
      {keys.map((key, i) => (
        <li key={key} className="flex items-center gap-1.5">
          <Swatch color={seriesColor(i)} />
          {humanize(key)}
        </li>
      ))}
    </ul>
  );
}

function Swatch({ color }: { color: string }) {
  return (
    <span
      aria-hidden
      className="inline-block size-2.5 shrink-0 rounded-[3px]"
      style={{ background: color }}
    />
  );
}

function Axes({ plot, grid, ticks }: { plot: Plot; grid: GridLine[]; ticks: XTick[] }) {
  return (
    <>
      {grid.map((line) => (
        <g key={line.y}>
          <line
            x1={plot.left}
            x2={plot.right}
            y1={line.y}
            y2={line.y}
            className="stroke-border"
            strokeWidth={1}
          />
          <text
            x={plot.left - 8}
            y={line.y}
            dy="0.32em"
            textAnchor="end"
            className="fill-muted-foreground text-[11px] tabular-nums"
          >
            {line.label}
          </text>
        </g>
      ))}
      {ticks.map((tick) => (
        <text
          key={`${tick.x}:${tick.label}`}
          x={tick.x}
          y={plot.bottom + 16}
          textAnchor={tick.anchor}
          className="fill-muted-foreground text-[11px]"
        >
          {tick.label}
        </text>
      ))}
    </>
  );
}

interface TooltipLine {
  key: string;
  color: string;
  name: string;
  value: number;
}

function Tooltip({
  x,
  width,
  header,
  lines,
}: {
  x: number;
  width: number;
  header: string;
  lines: TooltipLine[];
}) {
  const flip = x > width / 2;
  return (
    <div
      className="pointer-events-none absolute top-2 z-10 rounded-md border border-border bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-md"
      style={{
        left: x + (flip ? -8 : 8),
        transform: flip ? "translateX(-100%)" : undefined,
      }}
    >
      <div className="mb-0.5 font-medium">{header}</div>
      {lines.map((line) => (
        <div key={line.key} className="flex items-center gap-1.5">
          <Swatch color={line.color} />
          <span className="text-muted-foreground">{line.name}</span>
          <span className="ml-3 tabular-nums">{formatNumber(line.value)}</span>
        </div>
      ))}
    </div>
  );
}

/* the numbers for a reader who cannot see the marks */
function ChartTable({
  caption,
  headers,
  rows,
}: {
  caption: string;
  headers: string[];
  rows: Array<Array<string | number>>;
}) {
  return (
    <table className="sr-only">
      <caption>{caption}</caption>
      <thead>
        <tr>
          {headers.map((header) => (
            <th key={header}>{header}</th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((row, i) => (
          <tr key={i}>
            {row.map((cell, j) => (
              <td key={j}>{cell}</td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

const plain = (value: ReactNode, fallback: string) =>
  typeof value === "string" ? value : fallback;

function nearest(positions: number[], x: number): number {
  let best = 0;
  for (let i = 1; i < positions.length; i++) {
    if (Math.abs(positions[i]! - x) < Math.abs(positions[best]! - x)) best = i;
  }
  return best;
}

export interface LineChartProps {
  /** one object per point */
  data: ChartRow[];
  /** the field along the bottom; dates are placed by time and sorted */
  x: string;
  /** the field to plot, or several for several lines (up to eight) */
  y: string | string[];
  title?: ReactNode;
  /** in px, axes included; default 220 */
  height?: number;
}

export function LineChart({
  data,
  x,
  y,
  title,
  height = DEFAULT_HEIGHT,
}: LineChartProps) {
  const [ref, width] = useWidth();
  const [hover, setHover] = useState<number | null>(null);
  const keys = seriesKeys(y);
  const model = useMemo(
    () => lineModel(data, x, keys, width, height),
    [data, x, keys.join("\u0000"), width, height],
  );
  const name = plain(title, "Line chart");
  return (
    <ChartCard title={title} empty={model === null}>
      {model && (
        <>
          <Legend keys={keys} />
          <div ref={ref} className="relative">
            <svg
              role="img"
              aria-label={name}
              width={width}
              height={height}
              onPointerMove={(event) => {
                const box = event.currentTarget.getBoundingClientRect();
                setHover(nearest(model.xs, event.clientX - box.left));
              }}
              onPointerLeave={() => setHover(null)}
            >
              <Axes plot={model.plot} grid={model.grid} ticks={model.ticks} />
              {hover !== null && (
                <line
                  x1={model.xs[hover]}
                  x2={model.xs[hover]}
                  y1={model.plot.top}
                  y2={model.plot.bottom}
                  className="stroke-muted-foreground"
                  strokeWidth={1}
                />
              )}
              {model.series.map((points, i) => {
                const last = lastPoint(points);
                return (
                  <g key={keys[i]}>
                    <path
                      d={linePath(points)}
                      fill="none"
                      stroke={seriesColor(i)}
                      strokeWidth={2}
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                    {last && (
                      <circle
                        cx={last.x}
                        cy={last.y}
                        r={4}
                        fill={seriesColor(i)}
                        stroke={SURFACE}
                        strokeWidth={2}
                      />
                    )}
                    {hover !== null && points[hover] && (
                      <circle
                        cx={points[hover]!.x}
                        cy={points[hover]!.y}
                        r={4}
                        fill={seriesColor(i)}
                        stroke={SURFACE}
                        strokeWidth={2}
                      />
                    )}
                  </g>
                );
              })}
            </svg>
            {hover !== null && (
              <Tooltip
                x={model.xs[hover]!}
                width={width}
                header={model.labels[hover]!}
                lines={keys.flatMap((key, i) => {
                  const point = model.series[i]![hover];
                  return point
                    ? [
                        {
                          key,
                          color: seriesColor(i),
                          name: humanize(key),
                          value: point.value,
                        },
                      ]
                    : [];
                })}
              />
            )}
          </div>
          <ChartTable
            caption={name}
            headers={[humanize(x), ...keys.map(humanize)]}
            rows={model.rows.map((row, i) => [
              model.labels[i]!,
              ...keys.map((key) => numeric(row[key]) ?? "—"),
            ])}
          />
        </>
      )}
    </ChartCard>
  );
}

export interface BarChartProps {
  /** one object per category */
  data: ChartRow[];
  /** the category field along the bottom */
  x: string;
  /** the field to plot, or several for grouped bars (up to eight) */
  y: string | string[];
  title?: ReactNode;
  /** in px, axes included; default 220 */
  height?: number;
}

export function BarChart({
  data,
  x,
  y,
  title,
  height = DEFAULT_HEIGHT,
}: BarChartProps) {
  const [ref, width] = useWidth();
  const [hover, setHover] = useState<number | null>(null);
  const keys = seriesKeys(y);
  const model = useMemo(
    () => barModel(data, x, keys, width, height),
    [data, x, keys.join("\u0000"), width, height],
  );
  const name = plain(title, "Bar chart");
  return (
    <ChartCard title={title} empty={model === null}>
      {model && (
        <>
          <Legend keys={keys} />
          <div ref={ref} className="relative">
            <svg
              role="img"
              aria-label={name}
              width={width}
              height={height}
              onPointerLeave={() => setHover(null)}
            >
              <Axes plot={model.plot} grid={model.grid} ticks={model.ticks} />
              {model.bars.map((group, i) => (
                <g key={i}>
                  {hover === i && (
                    <rect
                      x={model.bands[i]!.x}
                      y={model.plot.top}
                      width={model.bands[i]!.width}
                      height={model.plot.bottom - model.plot.top}
                      className="fill-muted"
                    />
                  )}
                  {group.map((bar) => (
                    <path
                      key={bar.series}
                      d={barPath(bar, model.baseline)}
                      fill={seriesColor(bar.series)}
                    />
                  ))}
                </g>
              ))}
              {model.bands.map((band, i) => (
                <rect
                  key={i}
                  x={band.x}
                  y={model.plot.top}
                  width={band.width}
                  height={model.plot.bottom - model.plot.top}
                  fill="transparent"
                  onPointerEnter={() => setHover(i)}
                />
              ))}
            </svg>
            {hover !== null && (
              <Tooltip
                x={model.bands[hover]!.x + model.bands[hover]!.width / 2}
                width={width}
                header={model.labels[hover]!}
                lines={model.bars[hover]!.map((bar) => ({
                  key: keys[bar.series]!,
                  color: seriesColor(bar.series),
                  name: humanize(keys[bar.series]!),
                  value: bar.value,
                }))}
              />
            )}
          </div>
          <ChartTable
            caption={name}
            headers={[humanize(x), ...keys.map(humanize)]}
            rows={data.map((row, i) => [
              model.labels[i]!,
              ...keys.map((key) => numeric(row[key]) ?? "—"),
            ])}
          />
        </>
      )}
    </ChartCard>
  );
}

export interface DonutChartProps {
  /** one object per slice; past seven, the smallest fold into "Other" */
  data: ChartRow[];
  /** the field that names a slice */
  label: string;
  /** the field that sizes a slice */
  value: string;
  title?: ReactNode;
  /** in px; default 220 */
  height?: number;
}

export function DonutChart({
  data,
  label,
  value,
  title,
  height = DEFAULT_HEIGHT,
}: DonutChartProps) {
  const [hover, setHover] = useState<number | null>(null);
  const slices = useMemo(() => donutSlices(data, label, value), [data, label, value]);
  const total = slices.reduce((sum, slice) => sum + slice.value, 0);
  /* the ring leaves room for its legend beside it in a two-up card */
  const ring = Math.round(height * 0.73);
  const center = ring / 2;
  const outer = center - 2;
  const inner = outer * 0.64;
  const name = plain(title, "Donut chart");
  let angle = 0;
  const shown = hover === null ? null : slices[hover]!;
  return (
    <ChartCard title={title} empty={slices.length === 0}>
      <div
        className="flex flex-wrap items-center justify-center gap-x-6 gap-y-3"
        style={{ minHeight: height }}
      >
        <svg
          role="img"
          aria-label={name}
          width={ring}
          height={ring}
          className="shrink-0"
          onPointerLeave={() => setHover(null)}
        >
          {slices.map((slice, i) => {
            const start = angle;
            angle += slice.share * Math.PI * 2;
            return (
              <path
                key={slice.label}
                d={arcPath(center, outer, inner, start, angle)}
                fill={slice.color}
                stroke={SURFACE}
                strokeWidth={2}
                opacity={hover === null || hover === i ? 1 : 0.45}
                onPointerEnter={() => setHover(i)}
              />
            );
          })}
          <text
            x={center}
            y={center}
            textAnchor="middle"
            className="fill-heading text-[22px] font-semibold"
          >
            {formatNumber(shown ? shown.value : total)}
          </text>
          <text
            x={center}
            y={center + 18}
            textAnchor="middle"
            className="fill-muted-foreground text-[11px]"
          >
            {shown ? shown.label : "Total"}
          </text>
        </svg>
        <ul className="flex min-w-32 flex-1 flex-col gap-1.5 text-xs">
          {slices.map((slice, i) => (
            <li
              key={slice.label}
              className="flex items-center gap-2"
              onPointerEnter={() => setHover(i)}
              onPointerLeave={() => setHover(null)}
            >
              <Swatch color={slice.color} />
              <span className="truncate" title={slice.label}>
                {slice.label}
              </span>
              <span className="ml-auto shrink-0 text-muted-foreground tabular-nums">
                {formatNumber(slice.value)} · {Math.round(slice.share * 100)}%
              </span>
            </li>
          ))}
        </ul>
      </div>
      <ChartTable
        caption={name}
        headers={[humanize(label), humanize(value)]}
        rows={slices.map((slice) => [slice.label, slice.value])}
      />
    </ChartCard>
  );
}
