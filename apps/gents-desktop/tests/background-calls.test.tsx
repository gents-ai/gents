import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BackgroundedToolView,
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { workspace } from "@/app/workspace";
import { writeSession } from "../src/hooks/sessionStore";
import {
  backgroundCalls,
  crlf,
  nextWrite,
  stateOf,
  type BackgroundCall,
  resetBackgroundPlaces,
  briefStateOf,
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
    expect(screen.getByText("Nothing is running in the background.")).toBeVisible();
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
    expect(running.getByRole("button").textContent).toBe("npm run build");
    expect(finished.getByRole("button").textContent).toBe("make test");
    const [build] = running.getAllByRole("listitem");
    const [test] = finished.getAllByRole("listitem");
    expect(within(test!).getByTestId("background-state")).toHaveTextContent("3 min");
    expect(test!.firstElementChild).toHaveAttribute(
      "title",
      "make test\nFinished after 3 min",
    );
    await waitFor(() =>
      expect(within(build!).getByTestId("background-state")).toHaveTextContent("stuck"),
    );
    expect(build!.firstElementChild).toHaveAttribute(
      "title",
      "npm run build\nStopped responding 3 min ago",
    );
    expect(screen.queryByText("Nothing is running in the background.")).toBeNull();
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

  it("puts a short state at the end of a row", () => {
    expect(briefStateOf(call({ tool: command("a", "x") }), NOW)).toBe("12 min");
    expect(
      briefStateOf(
        call({
          tool: command("b", "x", { statusKind: "failed" }, {}),
        }),
        NOW,
      ),
    ).toMatch(/^(failed|exit \d+)$/);
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

  it("offers no stop for a native process", () => {
    const { app } = appWith([command("build", "npm run build")]);
    render(<BackgroundSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(screen.queryByRole("button", { name: /^Stop/ })).toBeNull();
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
