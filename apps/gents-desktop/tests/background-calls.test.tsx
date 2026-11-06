import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";
import type {
  BackgroundedToolView,
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { messagePlaces } from "@/app/messagePlaces";
import { workspace } from "@/app/workspace";
import { writeSession } from "../src/hooks/sessionStore";
import {
  backgroundCalls,
  crlf,
  nextWrite,
  stateOf,
  type BackgroundCall,
  resetBackgroundPlaces,
  rowStateOf,
  useBackgroundCalls,
} from "../src/ui/screens/background";
import { BackgroundButton } from "../src/ui/screens/BackgroundButton";
import { BackgroundSurface } from "../src/ui/screens/BackgroundCalls";
import { openTerminal } from "../src/ui/screens/terminal";
import { BackgroundToolButton } from "../src/ui/screens/BackgroundToolButton";
import { StartedSessionsButton } from "../src/ui/screens/StartedSessionsButton";
import { node, testApp, withApp } from "./app-fixture";

const writes = vi.hoisted(() => [] as { data: string; reset: boolean }[]);
const reading = vi.hoisted(() => ({ above: 0 }));
vi.mock("../src/ui/screens/terminal", () => ({
  openTerminal: vi.fn(async () => ({
    write: (data: string, reset: boolean) => writes.push({ data, reset }),
    linesAbove: () => reading.above,
    find: () => true,
    clearFind: () => {},
    dispose: () => {},
  })),
}));

const NODE = "did:key:parent";
const NOW = Date.parse("2026-10-26T12:00:00Z");
const minutesAgo = (mins: number) => new Date(NOW - mins * 60_000).toISOString();
const secondsAgo = (secs: number) => new Date(NOW - secs * 1000).toISOString();

const command = (
  toolCallId: string,
  line: string,
  over: Partial<RenderedToolCallView> = {},
  result: Record<string, unknown> = {},
): RenderedToolCallView =>
  ({
    itemKey: `item-${toolCallId}`,
    toolName: "bash",
    toolCallId,
    toolCallDocId: `doc-${toolCallId}`,
    requestId: "req-1",
    statusKind: "running",
    awaitMode: "background",
    startedAt: minutesAgo(12),
    presentation: {
      kind: "command",
      command: line,
      exitCode: null,
      timedOut: false,
      failed: false,
      durationMs: null,
      cwd: "/work",
      executionMode: null,
      networkMode: null,
      stdout: "",
      stderr: "",
      fallbackOutput: null,
      ...result,
    },
    ...over,
  }) as unknown as RenderedToolCallView;

const started = (toolCallId: string): RenderedToolCallView =>
  ({
    itemKey: `item-${toolCallId}`,
    toolName: "agent_new",
    toolCallId,
    requestId: "req-1",
    statusKind: "running",
    awaitMode: "background",
    presentation: { kind: "agent", action: "start", name: "crew", sessionId: null },
  }) as unknown as RenderedToolCallView;

const viewOf = (
  toolCallId: string,
  over: Partial<BackgroundedToolView> = {},
): BackgroundedToolView =>
  ({
    requestId: "req-1",
    toolCallId,
    toolCallDocId: `doc-${toolCallId}`,
    toolName: "bash",
    startsSession: false,
    job: false,
    lifecycleState: "running",
    status: null,
    startedAt: minutesAgo(12),
    ageMs: null,
    deadlineAt: null,
    deadlineExpired: false,
    awaitMode: "background",
    stuckSince: null,
    nativeExecutor: null,
    waitStatus: null,
    ...over,
  }) as BackgroundedToolView;

const group = (...tools: RenderedToolCallView[]): RenderedTimelineItem =>
  ({
    kind: "toolGroup",
    itemKey: `group-${tools[0]?.itemKey}`,
    messageSequence: 1,
    tools,
  }) as unknown as RenderedTimelineItem;

const snapshot = (...tools: RenderedToolCallView[]) =>
  ({
    sessionId: "parent",
    latestRequestId: "req-1",
    timelineItems: tools.length ? [group(...tools)] : [],
  }) as unknown as DesktopSessionSnapshot;

function appWith(
  tools: RenderedToolCallView[],
  views: BackgroundedToolView[] = [],
  api: object = {},
) {
  const fetchOperationsSnapshot = vi
    .fn()
    .mockResolvedValue({ backgroundedTools: views });
  const app = testApp({
    api: { fetchOperationsSnapshot, ...api },
    deployments: [node({ nodeDid: NODE, sessions: [] })],
    session: snapshot(...tools),
    selection: { nodeDid: NODE, sessionId: "parent" },
  });
  const current = app.stores.client.getState().snapshot!;
  app.stores.client.setState({
    snapshot: {
      ...current,
      client: { ...current.client!, operatorConfigAuthoritativeNodeDids: [NODE] },
    },
  });
  return { app, fetchOperationsSnapshot };
}

const call = (over: Partial<BackgroundCall>): BackgroundCall => ({
  key: "k",
  requestId: "req-1",
  toolCallId: "c",
  tool: null,
  view: null,
  runs: "npm run build",
  ...over,
});

describe("background calls", () => {
  it("keeps calls with the same logical identity separate by physical document", () => {
    const first = command("shared", "first", { toolCallDocId: "physical-a" });
    const second = command("shared", "second", { toolCallDocId: "physical-b" });
    const calls = backgroundCalls(
      [first, second],
      [viewOf("shared", { toolCallDocId: "physical-b", status: "second-only" })],
      new Set(["req-1"]),
    );
    expect(calls).toHaveLength(2);
    expect(new Set(calls.map((call) => call.key)).size).toBe(2);
    expect(
      calls.find((call) => call.tool?.toolCallDocId === "physical-a")?.view,
    ).toBeNull();
    expect(
      calls.find((call) => call.tool?.toolCallDocId === "physical-b")?.view?.status,
    ).toBe("second-only");
  });
  it("lists the session's backgrounded calls, not its started sessions or other sessions' calls", () => {
    const calls = backgroundCalls(
      [
        command("build", "npm run build"),
        command("quick", "ls", { awaitMode: "foreground" }),
        started("crew"),
      ],
      [viewOf("build"), viewOf("elsewhere", { requestId: "req-other" })],
      new Set(["req-1"]),
    );
    expect(calls.map((c) => c.runs)).toEqual(["npm run build"]);
  });

  it("names a spawned call by the command it was spawned with", () => {
    const spawn = {
      itemKey: "item-spawn",
      toolName: "spawn_process",
      toolCallId: "spawn",
      toolCallDocId: "doc-spawn",
      requestId: "req-1",
      statusKind: "success",
      awaitMode: "background",
      presentation: {
        kind: "process",
        action: "spawn",
        target: "bash",
        description: JSON.stringify({ command: "npm run dev" }),
        output: null,
      },
    } as unknown as RenderedToolCallView;
    const child = command("spawned:doc-spawn", "bash");
    const calls = backgroundCalls(
      [spawn, child],
      [viewOf("spawn", { toolName: "spawn_process" }), viewOf("spawned:doc-spawn")],
      new Set(["req-1"]),
    );
    expect(calls.map((c) => [c.toolCallId, c.runs])).toEqual([
      ["spawned:doc-spawn", "npm run dev"],
    ]);
  });

  it("lists a running call of the session whose row is not loaded by its tool name", () => {
    const calls = backgroundCalls(
      [],
      [viewOf("old", { toolName: "bash" })],
      new Set(["req-1"]),
    );
    expect(calls.map((c) => c.runs)).toEqual(["bash"]);
    expect(stateOf(calls[0]!, NOW).text).toBe("Running for 12 min");
  });

  it("reads how long a call has run and where it stands", () => {
    const running = command("build", "npm run build");
    expect(stateOf(call({ tool: running }), NOW).text).toBe("Running for 12 min");
    expect(
      stateOf(
        call({ tool: running, view: viewOf("build", { stuckSince: minutesAgo(3) }) }),
        NOW,
      ),
    ).toEqual({ tone: "stuck", text: "Stopped responding 3 min ago" });
    expect(
      stateOf(
        call({
          tool: running,
          view: viewOf("build", { deadlineAt: minutesAgo(5), deadlineExpired: true }),
        }),
        NOW,
      ),
    ).toEqual({ tone: "overdue", text: "Past its deadline by 5 min" });
    expect(
      stateOf(call({ tool: command("b", "x", { startedAt: minutesAgo(125) }) }), NOW)
        .text,
    ).toBe("Running for 2 h 5 min");
    expect(
      stateOf(
        call({
          tool: command(
            "b",
            "x",
            { statusKind: "success" },
            { durationMs: 90_000, exitCode: 0 },
          ),
        }),
        NOW,
      ),
    ).toEqual({ tone: "done", text: "Finished after 1 min" });
    expect(
      stateOf(
        call({
          tool: command(
            "b",
            "x",
            { statusKind: "success" },
            { failed: true, exitCode: 2 },
          ),
        }),
        NOW,
      ),
    ).toEqual({ tone: "failed", text: "Failed · exit code 2" });
    expect(stateOf(call({ view: viewOf("b") }), NOW).text).toBe("Running for 12 min");
  });

  it("writes only what is new when the tail extends what is shown", () => {
    expect(nextWrite("", "a\nb")).toEqual({ reset: true, data: "a\r\nb" });
    expect(nextWrite("a\nb", "a\nb\nc")).toEqual({ reset: false, data: "\r\nc" });
    expect(nextWrite("a\nb", "b\nc")).toEqual({ reset: true, data: "b\r\nc" });
    expect(nextWrite("same", "same")).toBeNull();
    expect(crlf("x\r\ny\nz")).toBe("x\r\ny\r\nz");
    expect(nextWrite("a\r", "a\r\nb")).toEqual({ reset: false, data: "\nb" });
  });
});

describe("the running in background group", () => {
  beforeEach(() => {
    resetBackgroundPlaces();
    workspace.reset();
    writes.length = 0;
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(NOW);
  });
  afterEach(() => vi.useRealTimers());

  it("is hidden when the session has nothing in the background", async () => {
    const { app, fetchOperationsSnapshot } = appWith([
      command("quick", "ls", { awaitMode: "foreground" }),
    ]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(screen.queryByRole("region", { name: /background/ })).toBeNull();
    expect(screen.getByText("Nothing running")).toBeVisible();
    expect(fetchOperationsSnapshot).not.toHaveBeenCalled();
  });

  it("lists each backgrounded call with what it runs and where it stands", async () => {
    const { app } = appWith(
      [
        command("build", "npm run build"),
        command(
          "test",
          "make test",
          { statusKind: "success" },
          { durationMs: 180_000 },
        ),
        started("crew"),
      ],
      [viewOf("build", { stuckSince: minutesAgo(3) })],
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    const running = within(screen.getByRole("region", { name: "Running" }));
    const finished = within(screen.getByRole("region", { name: "Finished" }));
    expect(running.getAllByRole("button")[0]!.textContent).toBe("npm run build");
    expect(finished.getAllByRole("button")[0]!.textContent).toBe("make test");
    const [build] = running.getAllByRole("listitem");
    const [test] = finished.getAllByRole("listitem");
    expect(within(test!).getByTestId("background-state")).toHaveTextContent(
      "Done · ran 3m",
    );
    expect(test!.firstElementChild).toHaveAttribute(
      "title",
      "make test\nFinished after 3 min",
    );
    await waitFor(() =>
      expect(within(build!).getByTestId("background-state")).toHaveTextContent(
        "Stuck · stopped responding 3 min ago",
      ),
    );
    expect(build!.firstElementChild).toHaveAttribute(
      "title",
      "npm run build\nStopped responding 3 min ago",
    );
    expect(screen.queryByText("Nothing running")).toBeNull();
  });

  it("opens a command's output from its row in the transcript", async () => {
    const tool = command("build", "npm run build", { partialOutputTail: "one" });
    const { app } = appWith([tool], [viewOf("build")]);
    render(<BackgroundToolButton tool={tool} />, { wrapper: withApp(app) });
    fireEvent.click(screen.getByRole("button", { name: "View output" }));
    expect(workspace.dock("session:parent")).toMatchObject({
      open: true,
      active: "background",
    });
    cleanup();

    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(screen.getByTestId("background-command")).toHaveTextContent("npm run build");
  });

  it("offers no output link on a command that ran in the foreground", () => {
    const tool = command("ls", "ls", { awaitMode: "foreground" });
    const { app } = appWith([tool]);
    render(<BackgroundToolButton tool={tool} />, { wrapper: withApp(app) });
    expect(screen.queryByRole("button", { name: "View output" })).toBeNull();
  });

  it("reopens the log the reader left, at their place, until they go Back", async () => {
    const tool = command("serve", "npm run dev", { partialOutputTail: "ready" });
    const { app } = appWith([tool], [viewOf("serve")]);
    const surface = () =>
      render(<BackgroundSurface placement="dock" sessionId="parent" />, {
        wrapper: withApp(app),
      });
    let view = surface();
    fireEvent.click(screen.getByRole("button", { name: "npm run dev" }));
    await waitFor(() => expect(openTerminal).toHaveBeenCalled());
    reading.above = 7;
    view.unmount();

    vi.mocked(openTerminal).mockClear();
    view = surface();
    expect(screen.getByTestId("background-command")).toHaveTextContent("npm run dev");
    await waitFor(() =>
      expect(openTerminal).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({ above: 7 }),
      ),
    );

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    view.unmount();
    surface();
    expect(screen.queryByTestId("background-command")).toBeNull();
    reading.above = 0;
  });

  it("goes Back to the row it opened, where the list was, and keeps that place", () => {
    const { app } = appWith(
      ["one", "two", "three"].map((id) =>
        command(id, `run ${id}`, { statusKind: "success" }),
      ),
    );
    const surface = () =>
      render(<BackgroundSurface placement="dock" sessionId="parent" />, {
        wrapper: withApp(app),
      });
    let view = surface();
    const viewport = () =>
      view.container.querySelector<HTMLElement>("[data-slot=scroll-area-viewport]")!;
    viewport().scrollTop = 120;
    fireEvent.scroll(viewport());

    fireEvent.click(screen.getByRole("button", { name: "run two" }));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(viewport().scrollTop).toBe(120);
    const returned = view.container.querySelectorAll("[data-returned]");
    expect(returned).toHaveLength(1);
    expect(returned[0]).toHaveClass("bg-accent");
    expect(returned[0]).toContainElement(
      screen.getByRole("button", { name: "run two" }),
    );

    /* another tab or the dock closed, then back */
    view.unmount();
    view = surface();
    expect(viewport().scrollTop).toBe(120);
    expect(view.container.querySelector("[data-returned]")).toBeNull();
  });

  it("says under each command where it stands, how long it ran and when it ended", () => {
    const line = (c: BackgroundCall) => {
      const { state, more } = rowStateOf(c, NOW);
      return more ? `${state} · ${more}` : state;
    };
    const ended = (
      id: string,
      statusKind: string,
      result: Record<string, unknown>,
      over: Partial<RenderedToolCallView> = {},
    ) =>
      call({
        tool: command(
          id,
          "x",
          { statusKind, completedAt: minutesAgo(2), ...over },
          result,
        ),
      });
    expect(line(call({ tool: command("a", "x", { startedAt: secondsAgo(42) }) }))).toBe(
      "Running · 42s",
    );
    expect(
      line(
        call({ view: viewOf("w", { waitStatus: { lifecycle: "pending" } as never }) }),
      ),
    ).toBe("Waiting to start");
    expect(line(ended("f", "failed", { durationMs: 38_000 }))).toBe(
      "Failed · ran 38s · 2 min ago",
    );
    expect(line(ended("e", "error", { durationMs: 38_000, exitCode: 1 }))).toBe(
      "Exit 1 · ran 38s · 2 min ago",
    );
    expect(
      line(
        ended(
          "t",
          "failed",
          { durationMs: 300_000, timedOut: true },
          { completedAt: minutesAgo(60) },
        ),
      ),
    ).toBe("Timed out · ran 5m · 1 h ago");
    expect(
      line(
        ended(
          "s",
          "cancelled",
          {},
          { startedAt: secondsAgo(12), completedAt: new Date(NOW).toISOString() },
        ),
      ),
    ).toBe("Stopped · ran 12s · just now");
    expect(
      line(
        ended("d", "success", { durationMs: 252_000 }, { completedAt: minutesAgo(60) }),
      ),
    ).toBe("Done · ran 4m 12s · 1 h ago");
    expect(
      line(
        call({
          tool: command("k", "x"),
          view: viewOf("k", { stuckSince: minutesAgo(3) }),
        }),
      ),
    ).toBe("Stuck · stopped responding 3 min ago");
  });

  it("counts each second a call runs", () => {
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    vi.setSystemTime(NOW);
    const { app } = appWith([
      command("serve", "npm run dev", { startedAt: secondsAgo(42) }),
      command(
        "done",
        "make",
        { statusKind: "success", completedAt: minutesAgo(1) },
        { durationMs: 4_000 },
      ),
    ]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    const [serve, done] = screen.getAllByTestId("background-state");
    expect(serve).toHaveTextContent("Running · 42s");
    act(() => vi.advanceTimersByTime(1000));
    expect(serve).toHaveTextContent("Running · 43s");
    expect(done).toHaveTextContent("Done · ran 4s · 1 min ago");
  });

  it("shows a call where it started in the transcript, from its row and its log", () => {
    const tool = command("build", "npm run build", { partialOutputTail: "one" });
    const { app } = appWith([tool]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    const place = {
      sessionId: "parent",
      sequence: 1,
      itemKey: "item-build",
      open: true,
    };
    fireEvent.click(screen.getByRole("button", { name: "Show in transcript" }));
    expect(messagePlaces.request()).toMatchObject(place);
    act(() => messagePlaces.landed(messagePlaces.request()!.id, false));

    fireEvent.click(screen.getByRole("button", { name: "npm run build" }));
    expect(screen.getByTestId("background-command")).toHaveTextContent("npm run build");
    fireEvent.click(screen.getByRole("button", { name: "Show in transcript" }));
    expect(messagePlaces.request()).toMatchObject(place);
    act(() => messagePlaces.landed(messagePlaces.request()!.id, false));
  });

  it("shows a spawned process at the step that spawned it", () => {
    const spawn = {
      ...command("spawn", "x", { toolName: "spawn_process", awaitMode: null }),
      presentation: {
        kind: "process",
        action: "spawn",
        target: "bash",
        description: JSON.stringify({ command: "npm run dev" }),
        output: null,
      },
    } as unknown as RenderedToolCallView;
    const { app } = appWith([spawn, command("spawned:doc-spawn", "bash")]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(screen.getByRole("button", { name: "Show in transcript" }));
    expect(messagePlaces.request()).toMatchObject({
      itemKey: "item-spawn",
      sequence: 1,
    });
    act(() => messagePlaces.landed(messagePlaces.request()!.id, false));
  });

  it("offers no way to the transcript for a call its rows can't place", async () => {
    const { app } = appWith(
      [command("build", "npm run build")],
      [viewOf("build"), viewOf("unloaded", { toolName: "cargo" })],
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    const unloaded = (await screen.findByRole("button", { name: "cargo" })).closest(
      "li",
    )!;
    const build = screen.getByRole("button", { name: "npm run build" }).closest("li")!;
    expect(
      within(build).getByRole("button", { name: "Show in transcript" }),
    ).toBeInTheDocument();
    expect(
      within(unloaded).queryByRole("button", { name: "Show in transcript" }),
    ).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "cargo" }));
    expect(screen.getByRole("button", { name: "Back" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Show in transcript" })).toBeNull();
  });

  it("says a finished spawned process's output can't be shown, without a terminal", () => {
    const child = command("spawned:doc-spawn", "bash", { statusKind: "success" });
    const { app } = appWith([child]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(screen.getAllByRole("button")[0]!);
    expect(screen.getByTestId("background-no-output")).toHaveTextContent(
      "The output isn't available after this process ended.",
    );
    expect(screen.queryByTestId("background-output")).toBeNull();
    expect(screen.queryByText("Output (last part)")).toBeNull();
  });

  it("marks where each day starts among finished calls, only once they span days", () => {
    const done = (id: string, startedAt: string) =>
      command(id, `echo ${id}`, { statusKind: "success", startedAt });
    const { app } = appWith([
      done("today", minutesAgo(30)),
      done("earlier", minutesAgo(60 * 24 * 3)),
    ]);
    const view = render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(screen.getAllByTestId("day-marker")).toHaveLength(2);
    view.unmount();

    const same = appWith([done("one", minutesAgo(30)), done("two", minutesAgo(40))]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(same.app),
    });
    expect(screen.queryByTestId("day-marker")).toBeNull();
  });

  it("says a finished command printed nothing", () => {
    const tool = command("quiet", "true", { statusKind: "success" });
    const { app } = appWith([tool]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(screen.getByRole("button", { name: "true" }));
    expect(screen.getByTestId("background-no-output")).toHaveTextContent(
      "It finished without printing anything.",
    );
  });

  it("opens a call's output and appends what it writes next", async () => {
    const tool = command("build", "npm run build", { partialOutputTail: "one\ntwo" });
    const { app } = appWith([tool], [viewOf("build")]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(screen.getByRole("button", { name: "npm run build" }));
    expect(screen.getByTestId("background-command")).toHaveTextContent("npm run build");
    expect(screen.getByText("Output (last part)")).toBeVisible();
    await waitFor(() => expect(writes).toEqual([{ data: "one\r\ntwo", reset: true }]));

    act(() =>
      writeSession(
        app.stores.session,
        snapshot({ ...tool, partialOutputTail: "one\ntwo\nthree" }),
      ),
    );
    await waitFor(() => expect(writes).toHaveLength(2));
    expect(writes[1]).toEqual({ data: "\r\nthree", reset: false });

    act(() =>
      writeSession(
        app.stores.session,
        snapshot({ ...tool, partialOutputTail: "three\nfour" }),
      ),
    );
    await waitFor(() => expect(writes).toHaveLength(3));
    expect(writes[2]).toEqual({ data: "three\r\nfour", reset: true });

    act(() =>
      writeSession(
        app.stores.session,
        snapshot({
          ...tool,
          statusKind: "error",
          partialOutputTail: null,
          presentation: { ...tool.presentation, exitCode: 1, failed: true } as never,
        }),
      ),
    );
    expect(await screen.findByText("Failed · exit code 1")).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByRole("region", { name: "Finished" })).toBeVisible();
    expect(screen.queryByRole("region", { name: "Running" })).toBeNull();
  });

  it("stops a running job through job control", async () => {
    const controlJob = vi.fn().mockResolvedValue({ Applied: {} });
    const { app } = appWith(
      [
        command("job", "nightly-report", {
          jobState: "running",
          toolName: "report",
          nodeDid: NODE,
        }),
      ],
      [],
      { jobControl: controlJob },
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(screen.getByRole("button", { name: "Stop nightly-report" }));
    expect(controlJob).toHaveBeenCalledWith({
      nodeDid: NODE,
      toolCallDocId: "doc-job",
      action: "stop",
    });
  });

  it("stops only the displayed native invocation", async () => {
    const processControl = vi
      .fn()
      .mockResolvedValue({ cancelled: { live_execution_cancelled: true } });
    const { app } = appWith(
      [command("build", "npm run build", { nodeDid: NODE })],
      [viewOf("build")],
      { processControl },
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Stop npm run build" }));
    expect(processControl).toHaveBeenCalledWith({
      nodeDid: NODE,
      sessionId: "parent",
      toolCallId: "build",
      toolCallDocId: "doc-build",
    });
  });

  it("stops the selected physical twin", async () => {
    const processControl = vi
      .fn()
      .mockResolvedValue({ cancelled: { live_execution_cancelled: true } });
    const { app } = appWith(
      [
        command("shared", "first command", {
          nodeDid: NODE,
          itemKey: "a",
          toolCallDocId: "physical-a",
        }),
        command("shared", "second command", {
          nodeDid: NODE,
          itemKey: "b",
          toolCallDocId: "physical-b",
        }),
      ],
      [
        viewOf("shared", { toolCallDocId: "physical-a" }),
        viewOf("shared", { toolCallDocId: "physical-b" }),
      ],
      { processControl },
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Stop second command" }));
    expect(processControl).toHaveBeenCalledWith({
      nodeDid: NODE,
      sessionId: "parent",
      toolCallId: "shared",
      toolCallDocId: "physical-b",
    });
  });

  it("reports an unverified stop without claiming success", async () => {
    const warning = vi.spyOn(toast, "warning");
    const success = vi.spyOn(toast, "success");
    const processControl = vi.fn().mockResolvedValue("unverified");
    const { app } = appWith(
      [command("build", "npm run build", { nodeDid: NODE })],
      [viewOf("build")],
      { processControl },
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Stop npm run build" }));
    await waitFor(() =>
      expect(warning).toHaveBeenCalledWith(
        "Could not confirm that npm run build stopped.",
      ),
    );
    expect(success).not.toHaveBeenCalled();
    warning.mockRestore();
    success.mockRestore();
  });

  it("does not offer native Stop without a physical row", () => {
    const { app } = appWith([
      command("build", "npm run build", { nodeDid: NODE, toolCallDocId: null }),
    ]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(screen.queryByRole("button", { name: /^Stop/ })).toBeNull();
  });

  it("submits native Stop from an enrolled client", async () => {
    const processControl = vi
      .fn()
      .mockResolvedValue({ Queued: { intent_doc_id: "intent" } });
    const { app } = appWith(
      [command("build", "npm run build", { nodeDid: NODE })],
      [viewOf("build")],
      { processControl },
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(
      await screen.findByRole("button", { name: "Stop npm run build" }),
    ).toBeVisible();
    const current = app.stores.client.getState().snapshot!;
    act(() =>
      app.stores.client.setState({
        snapshot: {
          ...current,
          client: { ...current.client!, operatorConfigAuthoritativeNodeDids: [] },
        },
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Stop npm run build" }));
    await waitFor(() => expect(processControl).toHaveBeenCalledOnce());
  });

  it("waits for call classification before offering native Stop", async () => {
    const { app, fetchOperationsSnapshot } = appWith([
      command("unknown", "pending call", { nodeDid: NODE, jobState: null }),
    ]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    await waitFor(() => expect(fetchOperationsSnapshot).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: "Stop pending call" })).toBeNull();
  });

  it("does not route an unclassified pending job to process control", async () => {
    const processControl = vi.fn();
    const controlJob = vi.fn().mockResolvedValue({ Applied: {} });
    const { app } = appWith(
      [command("job", "nightly-report", { nodeDid: NODE, jobState: null })],
      [viewOf("job", { toolName: "report", job: true })],
      { processControl, jobControl: controlJob },
    );
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    fireEvent.click(await screen.findByRole("button", { name: "Stop nightly-report" }));
    expect(controlJob).toHaveBeenCalledWith({
      nodeDid: NODE,
      toolCallDocId: "doc-job",
      action: "stop",
    });
    expect(processControl).not.toHaveBeenCalled();
  });
});

describe("reading running calls again", () => {
  const showWindow = (shown: boolean) =>
    act(() => {
      Object.defineProperty(document, "visibilityState", {
        configurable: true,
        get: () => (shown ? "visible" : "hidden"),
      });
      document.dispatchEvent(new Event("visibilitychange"));
    });

  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    vi.setSystemTime(NOW);
  });
  afterEach(() => {
    Reflect.deleteProperty(document, "visibilityState");
    vi.useRealTimers();
  });

  it("waits while the window is hidden and reads at once when it shows", async () => {
    const { app, fetchOperationsSnapshot } = appWith(
      [command("build", "npm run build")],
      [viewOf("build")],
    );
    renderHook(() => useBackgroundCalls("parent"), { wrapper: withApp(app) });
    expect(fetchOperationsSnapshot).toHaveBeenCalledTimes(1);
    await act(() => vi.advanceTimersByTimeAsync(10_000));
    expect(fetchOperationsSnapshot).toHaveBeenCalledTimes(2);

    showWindow(false);
    await act(() => vi.advanceTimersByTimeAsync(60_000));
    expect(fetchOperationsSnapshot).toHaveBeenCalledTimes(2);

    showWindow(true);
    expect(fetchOperationsSnapshot).toHaveBeenCalledTimes(3);
    await act(() => vi.advanceTimersByTimeAsync(10_000));
    expect(fetchOperationsSnapshot).toHaveBeenCalledTimes(4);
  });
});

describe("the background button", () => {
  beforeEach(() => workspace.reset());

  it("counts what runs in the background and opens the Background tab", () => {
    const { app } = appWith([command("build", "npm run build")]);
    render(<BackgroundButton sessionId="parent" />, { wrapper: withApp(app) });
    const button = screen.getByRole("button", { name: "1 running in background" });
    expect(button).toHaveTextContent("1");
    fireEvent.click(button);
    expect(workspace.dock("session:parent")).toMatchObject({
      open: true,
      active: "background",
    });
  });

  it("hides once nothing runs, unless its tab is open", () => {
    const { app } = appWith([command("build", "x", { statusKind: "success" })]);
    const { unmount } = render(<BackgroundButton sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(screen.queryByRole("button")).toBeNull();
    unmount();
    workspace.openSurface("session:parent", "background");
    render(<BackgroundButton sessionId="parent" />, { wrapper: withApp(app) });
    expect(
      screen.getByRole("button", { name: "0 running in background" }),
    ).toBeInTheDocument();
  });

  it("leaves the started sessions button to started sessions", () => {
    const { app } = appWith([command("build", "npm run build")]);
    render(<StartedSessionsButton sessionId="parent" />, { wrapper: withApp(app) });
    expect(screen.queryByRole("button")).toBeNull();
  });
});
