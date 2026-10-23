import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { parseTranscriptDirective, resolveDirective } from "@/contrib/directives";
import { registry } from "@/contrib/registry";
import { TRANSCRIPT_DIRECTIVE_AREA } from "@/contrib/types";
import { Markdown } from "@/screens/Markdown";
import { TranscriptPanel } from "@/screens/Transcript";
import { NO_PARENT } from "@/screens/parentWork";
import { NO_WORKERS } from "@/screens/workers";
import { renderIn, testApp } from "./app-fixture";
import { assistantMessage, sessionSnapshot } from "./timeline-fixture";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

/* a plugin that claims `::hello` for the span of one test */
function claimHello() {
  disposers.push(
    registry.register({
      id: "t:hello",
      area: TRANSCRIPT_DIRECTIVE_AREA,
      source: "plugin:t",
      data: {
        name: "hello",
        render: ({ attrs }: { attrs: Readonly<Record<string, string>> }) => (
          <b data-testid="hello-card">hi {attrs.name}</b>
        ),
      },
    }),
  );
}

const disposers: Array<() => void> = [];
afterEach(() => {
  disposers.splice(0).forEach((dispose) => dispose());
  vi.restoreAllMocks();
});

describe("parseTranscriptDirective", () => {
  it("parses a bare directive and one with attributes", () => {
    expect(parseTranscriptDirective("::board")).toEqual({
      name: "board",
      attrs: {},
      source: "::board",
    });
    expect(parseTranscriptDirective(`  ::board{id="42" lane='triage'}  `)).toEqual({
      name: "board",
      attrs: { id: "42", lane: "triage" },
      source: `::board{id="42" lane='triage'}`,
    });
  });

  it("refuses prose, multi-line text, bad names and oversize input", () => {
    expect(parseTranscriptDirective("see ::board for details")).toBeNull();
    expect(parseTranscriptDirective("::board\nmore")).toBeNull();
    expect(parseTranscriptDirective("::Board")).toBeNull();
    expect(parseTranscriptDirective("std::vector")).toBeNull();
    expect(parseTranscriptDirective("::b{" + "x".repeat(2000) + "}")).toBeNull();
  });

  it("lower-cases attribute keys", () => {
    expect(parseTranscriptDirective(`::x{Name="A"}`)?.attrs).toEqual({ name: "A" });
  });
});

describe("resolveDirective", () => {
  it("is first-registration-wins on a name collision", () => {
    disposers.push(
      registry.register({
        id: "p1:d",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        source: "plugin:p1",
        data: { name: "dup", render: () => "first" },
      }),
    );
    disposers.push(
      registry.register({
        id: "p2:d",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        source: "plugin:p2",
        data: { name: "dup", render: () => "second" },
      }),
    );
    expect(
      resolveDirective("dup")?.render({ attrs: {}, source: "", streaming: false }),
    ).toBe("first");
  });

  it("is null for an unclaimed name", () => {
    expect(resolveDirective("nobody")).toBeNull();
  });
});

describe("Markdown directive rendering", () => {
  it("renders a claimed whole-paragraph directive as the plugin's component", () => {
    disposers.push(
      registry.register({
        id: "t:hello",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        source: "plugin:t",
        data: {
          name: "hello",
          render: ({ attrs }: { attrs: Readonly<Record<string, string>> }) => (
            <b data-testid="hello-card">hi {attrs.name}</b>
          ),
        },
      }),
    );
    render(
      <Markdown
        directives
      >{`Intro line.\n\n::hello{name="world"}\n\nAfter.`}</Markdown>,
    );
    expect(screen.getByTestId("hello-card")).toHaveTextContent("hi world");
    expect(screen.getByTestId("transcript-directive")).toHaveAttribute(
      "data-directive",
      "hello",
    );
    expect(screen.getByText("Intro line.")).toBeInTheDocument();
  });

  it("is off by default: a claimed directive stays prose where nothing opted in", () => {
    claimHello();
    render(<Markdown>{`::hello{name="world"}`}</Markdown>);
    expect(screen.queryByTestId("hello-card")).toBeNull();
    expect(screen.getByText(`::hello{name="world"}`)).toBeInTheDocument();
    /* the mailbox's shape: soft breaks on, directives not asked for */
    render(<Markdown breaks>{`::hello{name="peer"}`}</Markdown>);
    expect(screen.queryByTestId("hello-card")).toBeNull();
  });

  it("leaves an unclaimed directive as the prose it was", () => {
    render(<Markdown directives>{`::nobody{x="1"}`}</Markdown>);
    expect(screen.queryByTestId("transcript-directive")).toBeNull();
    expect(screen.getByText(`::nobody{x="1"}`)).toBeInTheDocument();
  });

  it("leaves a directive embedded in prose alone", () => {
    disposers.push(
      registry.register({
        id: "t:inline",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        source: "plugin:t",
        data: { name: "inline", render: () => <i data-testid="no" /> },
      }),
    );
    render(<Markdown directives>{`Try ::inline{} here.`}</Markdown>);
    expect(screen.queryByTestId("no")).toBeNull();
  });

  it("contains a throwing directive renderer in an error chip", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    disposers.push(
      registry.register({
        id: "t:boom",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        source: "plugin:t",
        data: {
          name: "boom",
          render: () => {
            throw new Error("kaboom");
          },
        },
      }),
    );
    render(<Markdown directives>{`Before.\n\n::boom\n\nAfter.`}</Markdown>);
    expect(screen.getByTestId("contrib-error-chip")).toHaveAttribute(
      "title",
      "directive:boom: kaboom",
    );
    expect(screen.getByText("After.")).toBeInTheDocument();
  });
});

describe("where a directive may render", () => {
  const panel = (items: RenderedTimelineItem[]) =>
    renderIn(
      testApp(),
      <TranscriptPanel
        inFlight={false}
        stopping={false}
        scroller={null}
        session={sessionSnapshot({ timelineItems: items })}
        workers={NO_WORKERS}
        parentWork={NO_PARENT}
        workerActions={{} as never}
      />,
    );

  it("renders in the model's reply", () => {
    claimHello();
    panel([
      assistantMessage({
        itemKey: "a1",
        sequence: 1,
        content: `::hello{name="reply"}`,
      }),
    ]);
    expect(screen.getByTestId("hello-card")).toHaveTextContent("hi reply");
  });

  it("stays prose in the model's reasoning", () => {
    claimHello();
    panel([
      assistantMessage({
        itemKey: "a1",
        sequence: 1,
        reasoning: `::hello{name="thought"}`,
        content: "Done.",
      }),
    ]);
    /* reasoning starts folded; unfold it so the text is really drawn */
    fireEvent.click(screen.getByRole("button", { name: /Thinking/ }));
    expect(screen.getByText(`::hello{name="thought"}`)).toBeInTheDocument();
    expect(screen.queryByTestId("hello-card")).toBeNull();
    expect(screen.getByText("Done.")).toBeInTheDocument();
  });
});
