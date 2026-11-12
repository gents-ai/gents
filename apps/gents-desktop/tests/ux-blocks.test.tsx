/* The dashboard blocks of `@gents/ux-sdk`, rendered directly: what each one
   does with the rows a plugin hands it. */
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  BarChart,
  DonutChart,
  ErrorState,
  LastUpdated,
  LineChart,
  Select,
  Stat,
  Table,
  Tabs,
} from "@/sandbox/sdk-exports";
import {
  barModel,
  barPath,
  donutSlices,
  lineModel,
  linePath,
  niceTicks,
  valueTicks,
} from "@/sandbox/blocks/chart-scale";

describe("Table", () => {
  const rows = [
    { name: "b", count: 2, note: "x" },
    { name: "a", count: 10, note: null },
    { name: "c", count: 1, note: "y" },
  ];
  const firstCells = (table: HTMLElement) =>
    [...table.querySelectorAll("tbody tr")].map(
      (tr) => tr.firstElementChild?.textContent,
    );

  it("sorts by a header, numbers as numbers, then the other way", () => {
    const { container } = render(<Table columns={["name", "count"]} rows={rows} />);
    fireEvent.click(screen.getByRole("button", { name: "Count" }));
    expect(firstCells(container)).toEqual(["c", "b", "a"]);
    expect(screen.getByRole("columnheader", { name: /Count/ })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );
    fireEvent.click(screen.getByRole("button", { name: /Count/ }));
    expect(firstCells(container)).toEqual(["a", "b", "c"]);
  });

  it("puts a row with nothing to sort by last in either direction", () => {
    const { container } = render(
      <Table
        columns={["name", "note"]}
        rows={rows}
        defaultSort={{ key: "note", direction: "desc" }}
      />,
    );
    expect(firstCells(container)).toEqual(["c", "b", "a"]);
  });

  it("prints a timestamp for a reader and sorts it by the raw value", () => {
    const { container } = render(
      <Table
        columns={["when"]}
        rows={[{ when: "2026-10-02T09:30:00Z" }, { when: "2026-10-01T09:30:00Z" }]}
        defaultSort={{ key: "when" }}
      />,
    );
    const cells = firstCells(container);
    expect(cells[0]).toMatch(/^Oct 1, /);
    expect(cells[1]).toMatch(/^Oct 2, /);
    expect(cells[0]).not.toContain("T09");
  });

  it("has a sticky header in a region of its own, and right-aligns numbers", () => {
    const { container } = render(
      <Table columns={["name", "count"]} rows={rows} height={200} />,
    );
    const region = container.querySelector("table")!.parentElement!;
    expect(region).toHaveStyle({ maxHeight: "200px" });
    expect(region.className).toContain("overflow-auto");
    const headers = container.querySelectorAll("th");
    expect(headers[0]!.className).toContain("sticky");
    expect(headers[1]!.className).toContain("text-right");
    expect(container.querySelector("table")!.className).toContain("tabular-nums");
  });

  it("offers no sort on a column that reads no field, and lets a cell draw itself", () => {
    render(
      <Table
        columns={[
          "name",
          {
            key: "actions",
            label: "Actions",
            render: (_v, row) => <b>go {String(row.name)}</b>,
          },
        ]}
        rows={rows.slice(0, 1)}
      />,
    );
    expect(screen.queryByRole("button", { name: "Actions" })).toBeNull();
    expect(screen.getByText("go b")).toBeInTheDocument();
  });

  it("shows the empty state instead of a bare header", () => {
    const { container } = render(
      <Table columns={["name"]} rows={[]} empty="None yet" />,
    );
    expect(screen.getByText("None yet")).toBeInTheDocument();
    expect(container.querySelector("table")).toBeNull();
  });

  it("draws the first 500 rows of a larger read and says so", () => {
    const many = Array.from({ length: 501 }, (_, i) => ({ id: i }));
    const { container } = render(<Table columns={["id"]} rows={many} />);
    expect(container.querySelectorAll("tbody tr")).toHaveLength(500);
    expect(screen.getByText("Showing the first 500 of 501 rows")).toBeInTheDocument();
  });
});

describe("Stat", () => {
  it("formats a number and points its delta the way the sign goes", () => {
    const { container } = render(
      <Stat label="Open" value={12900} delta={-3} tone="bad" />,
    );
    expect(container.textContent).toBe("Open12.9K↓ 3");
    expect(screen.getByText(/↓/).className).toContain("text-destructive");
  });

  it("reads the sign off a text delta and leaves other text alone", () => {
    const up = render(<Stat label="A" value="n/a" delta="+4% this week" tone="good" />);
    expect(up.container.textContent).toContain("↑ +4% this week");
    const flat = render(<Stat label="B" value={1} delta="flat" />);
    expect(flat.container.textContent).toContain("flat");
    expect(flat.container.textContent).not.toMatch(/[↑↓]/);
  });
});

describe("Tabs", () => {
  const items = [
    { value: "a", label: "First", content: <p>first panel</p> },
    { value: "b", label: "Second", content: <p>second panel</p> },
  ];

  it("opens the first tab and switches on click", async () => {
    render(<Tabs items={items} />);
    expect(screen.getByText("first panel")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Second" }));
    expect(screen.getByText("second panel")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("first panel")).toBeNull());
  });

  it("reports a change when it is controlled and stays where it is told", () => {
    const onChange = vi.fn();
    render(<Tabs items={items} value="a" onChange={onChange} />);
    fireEvent.click(screen.getByRole("tab", { name: "Second" }));
    expect(onChange).toHaveBeenCalledWith("b");
    expect(screen.getByText("first panel")).toBeInTheDocument();
  });
});

describe("Select", () => {
  it("shows the chosen option's label and reports a new choice", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <Select
        label="Lane"
        value="b"
        onChange={onChange}
        options={[{ value: "a", label: "Alpha" }, { value: "b", label: "Beta" }, "c"]}
      />,
    );
    const trigger = screen.getByRole("combobox", { name: "Lane" });
    expect(trigger).toHaveTextContent("Beta");
    await user.click(trigger);
    await user.click(await screen.findByRole("option", { name: "Alpha" }));
    expect(onChange).toHaveBeenCalledWith("a");
  });
});

describe("charts", () => {
  it("round their axes, and a flat series still has a scale", () => {
    expect(niceTicks(0, 97)).toEqual([0, 25, 50, 75, 100]);
    expect(niceTicks(0, 0).at(-1)).toBe(1);
    expect(valueTicks([250, 260], false)[0]).toBeGreaterThan(0);
    expect(valueTicks([250, 260], true)[0]).toBe(0);
    expect(valueTicks([40, 100], false)[0]).toBe(0);
    expect(valueTicks([-5, 10], true)[0]).toBeLessThan(0);
  });

  it("LineChart places dates by time, sorted, and breaks across a gap", () => {
    const model = lineModel(
      [
        { day: "2026-10-05", n: 3 },
        { day: "2026-10-01", n: 1 },
        { day: "2026-10-02", n: null },
      ],
      "day",
      ["n"],
      480,
      220,
    )!;
    const [first, gap, last] = model.series[0]!;
    expect(first!.value).toBe(1);
    expect(gap).toBeNull();
    expect(last!.value).toBe(3);
    /* one day of a four-day span sits a quarter of the way along */
    const span = model.plot.right - model.plot.left;
    expect(model.xs[1]! - model.plot.left).toBeCloseTo(span / 4);
    expect(linePath(model.series[0]!).match(/M/g)).toHaveLength(2);
  });

  it("read a date with no time as that day where the reader is", () => {
    const model = lineModel(
      [
        { day: "2026-10-01", n: 1 },
        { day: "2026-10-02", n: 2 },
      ],
      "day",
      ["n"],
      480,
      220,
    )!;
    expect(model.labels).toEqual(["Oct 1", "Oct 2"]);
    expect(model.ticks.map((t) => t.label)).toEqual(["Oct 1", "Oct 2"]);
  });

  it("thin a long x axis so its labels never overlap", () => {
    const data = Array.from({ length: 60 }, (_, i) => ({
      name: `category-${i}`,
      n: i,
    }));
    const model = barModel(data, "name", ["n"], 480, 220)!;
    expect(model.ticks.length).toBeLessThan(data.length);
    for (let i = 1; i < model.ticks.length; i++) {
      expect(model.ticks[i]!.x - model.ticks[i - 1]!.x).toBeGreaterThan(50);
    }
  });

  it("BarChart caps bar width, keeps a square foot, and hangs a negative below zero", () => {
    const model = barModel(
      [
        { k: "a", n: 5 },
        { k: "b", n: -5 },
      ],
      "k",
      ["n"],
      480,
      220,
    )!;
    const [up] = model.bars[0]!;
    const [down] = model.bars[1]!;
    expect(up!.width).toBe(24);
    expect(up!.y).toBeLessThan(model.baseline);
    expect(down!.y).toBeGreaterThan(model.baseline);
    expect(barPath(up!, model.baseline)).toMatch(
      new RegExp(`^M${up!.x} ${model.baseline}`),
    );
  });

  it("DonutChart drops nothing it can draw and folds past seven into Other", () => {
    const data = Array.from({ length: 9 }, (_, i) => ({ k: `s${i}`, n: i + 1 }));
    const slices = donutSlices(data, "k", "n");
    expect(slices).toHaveLength(7);
    expect(slices[6]!.label).toBe("Other");
    expect(slices[6]!.value).toBe(1 + 2 + 3);
    expect(slices.reduce((sum, s) => sum + s.share, 0)).toBeCloseTo(1);
    /* a slice keeps the color of its place in the data */
    expect(
      donutSlices(
        [
          { k: "a", n: 1 },
          { k: "b", n: 9 },
        ],
        "k",
        "n",
      )[0]!.color,
    ).toBe("var(--chart-1)");
  });

  it("render one svg and a table view, or say there is no data", () => {
    const { container } = render(
      <LineChart
        title="Trend"
        data={[
          { d: "a", x: 1 },
          { d: "b", x: 2 },
        ]}
        x="d"
        y={["x"]}
      />,
    );
    expect(screen.getByRole("img", { name: "Trend" })).toBeInTheDocument();
    expect(container.querySelector("table.sr-only tbody")!.children).toHaveLength(2);

    const empty = render(<BarChart title="None" data={[]} x="d" y="x" />);
    expect(within(empty.container).getByText("No data")).toBeInTheDocument();
    expect(empty.container.querySelector("svg")).toBeNull();

    const donut = render(
      <DonutChart title="D" data={[{ k: "a", n: 0 }]} label="k" value="n" />,
    );
    expect(within(donut.container).getByText("No data")).toBeInTheDocument();
  });
});

describe("states", () => {
  it("ErrorState retries through the handler it is given", () => {
    const onRetry = vi.fn();
    render(<ErrorState message="the node is away" onRetry={onRetry} />);
    expect(screen.getByRole("alert")).toHaveTextContent("the node is away");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("LastUpdated says how old the read is and when one is running", () => {
    const { rerender, container } = render(<LastUpdated at={Date.now() - 120_000} />);
    expect(container.textContent).toMatch(/^Updated 2 minutes ago$/);
    rerender(<LastUpdated at={Date.now() - 120_000} loading />);
    expect(container.textContent).toBe("Updating…");
    rerender(<LastUpdated at={null} />);
    expect(container.textContent).toBe("Not updated yet");
  });
});
