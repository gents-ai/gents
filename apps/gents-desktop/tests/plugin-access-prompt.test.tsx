import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

const { windowListeners, unlisten, registration, environment } = vi.hoisted(() => {
  const registration = {
    autoResolve: true,
    resolve: null as null | ((stop: () => void) => void),
    reject: null as null | ((reason?: unknown) => void),
  };
  return {
    windowListeners: new Map<string, () => void>(),
    unlisten: vi.fn(),
    registration,
    environment: { native: true },
  };
});
vi.mock("../src/lib/nativeShell", () => ({
  inNativeShell: () => environment.native,
  listenToWindow: (event: string, handler: () => void) => {
    windowListeners.set(event, handler);
    if (registration.autoResolve) return Promise.resolve(unlisten);
    return new Promise<() => void>((resolve, reject) => {
      registration.resolve = resolve;
      registration.reject = reject;
    });
  },
}));

import { PluginAccessPrompt } from "../src/ui/screens/PluginAccessPrompt";
import { renderIn, testApp } from "./app-fixture";

const pending = vi.fn();
const decide = vi.fn();
const subscribe = vi.fn();
const unsubscribe = vi.fn();
const show = () =>
  renderIn(
    testApp({
      api: {
        listPendingPluginApprovals: pending,
        decidePluginApproval: decide,
        subscribePluginApprovals: subscribe,
        unsubscribePluginApprovals: unsubscribe,
      },
    }),
    <PluginAccessPrompt />,
  );

const question = {
  id: "q1",
  prompt: "Allow team/ocr to read /data/x.pdf?",
  folder: "/data",
  isDir: false,
};

const announceQueueChange = () => {
  const handler = windowListeners.get("desktop://plugin-approvals-changed");
  if (!handler) throw new Error("the prompt never subscribed to the change event");
  handler();
};

describe("PluginAccessPrompt", () => {
  beforeEach(() => {
    pending.mockReset();
    decide.mockReset();
    decide.mockResolvedValue(undefined);
    subscribe.mockReset();
    subscribe.mockResolvedValue(undefined);
    unsubscribe.mockReset();
    unsubscribe.mockResolvedValue(undefined);
    vi.mocked(toast.error).mockClear();
    environment.native = true;
    windowListeners.clear();
    unlisten.mockReset();
    registration.autoResolve = true;
    registration.resolve = null;
    registration.reject = null;
  });

  it("shows nothing while no plugin is waiting", async () => {
    pending.mockResolvedValue({ requests: [] });
    show();
    await waitFor(() => expect(pending).toHaveBeenCalled());
    expect(screen.queryByText("Allow once")).toBeNull();
  });

  it("registers with native cryptography when randomUUID is unavailable", async () => {
    const nativeCrypto = crypto;
    vi.stubGlobal("crypto", {
      getRandomValues: nativeCrypto.getRandomValues.bind(nativeCrypto),
    });
    try {
      pending.mockResolvedValue({ requests: [question] });
      const view = show();
      expect(await screen.findByText(question.prompt)).toBeTruthy();
      expect(subscribe).toHaveBeenCalledExactlyOnceWith(expect.any(String));
      view.unmount();
      await waitFor(() =>
        expect(unsubscribe).toHaveBeenCalledExactlyOnceWith(
          subscribe.mock.calls[0]![0],
        ),
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("reads the queue once on mount and never on a timer", async () => {
    pending.mockResolvedValue({ requests: [] });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      show();
      await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
      act(() => vi.advanceTimersByTime(5000));
    } finally {
      vi.useRealTimers();
    }
    expect(pending).toHaveBeenCalledTimes(1);
  });

  it("shows a question the bridge announces after the mount read", async () => {
    pending.mockResolvedValueOnce({ requests: [] });
    pending.mockResolvedValue({ requests: [question] });
    show();
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    await act(async () => announceQueueChange());
    expect(await screen.findByText(question.prompt)).toBeTruthy();
    expect(pending).toHaveBeenCalledTimes(2);
  });

  it.each([
    ["Allow once", "once"],
    ["Always allow this file", "file"],
    ["Always allow this folder", "always"],
    ["Deny", "deny"],
  ])("sends %s as %s", async (label, decision) => {
    pending.mockResolvedValue({ requests: [question] });
    show();
    await screen.findByText(question.prompt);
    fireEvent.click(screen.getByText(label));
    await waitFor(() => expect(decide).toHaveBeenCalledWith("q1", decision));
  });

  it("does not bring back a request answered while a read was out", async () => {
    let late!: (value: { requests: (typeof question)[] }) => void;
    let reads = 0;
    pending.mockImplementation(async () => {
      reads += 1;
      if (reads === 1) return { requests: [question] };
      return new Promise((resolve) => (late = resolve));
    });
    show();
    await screen.findByText(question.prompt);
    await act(async () => announceQueueChange());
    expect(reads).toBe(2);

    fireEvent.click(screen.getByText("Allow once"));
    await waitFor(() => expect(screen.queryByText(question.prompt)).toBeNull());

    await act(async () => late({ requests: [question] }));
    expect(screen.queryByText(question.prompt)).toBeNull();
  });

  it("closes without answering when a question is answered elsewhere", async () => {
    pending.mockResolvedValueOnce({ requests: [question] });
    pending.mockResolvedValue({ requests: [] });
    show();
    await screen.findByText(question.prompt);
    await act(async () => announceQueueChange());
    await waitFor(() => expect(screen.queryByText(question.prompt)).toBeNull());
    expect(decide).not.toHaveBeenCalled();
  });

  it("stops listening when it unmounts", async () => {
    pending.mockResolvedValue({ requests: [] });
    const view = show();
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    view.unmount();
    expect(unlisten).toHaveBeenCalled();
    await waitFor(() =>
      expect(unsubscribe).toHaveBeenCalledWith(subscribe.mock.calls[0]![0]),
    );
  });

  it("reads only after the change listener is registered", async () => {
    pending.mockResolvedValue({ requests: [] });
    registration.autoResolve = false;
    show();
    await waitFor(() => expect(windowListeners.size).toBe(1));
    expect(pending).not.toHaveBeenCalled();
    expect(subscribe).not.toHaveBeenCalled();
    await act(async () => registration.resolve!(unlisten));
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    expect(subscribe).toHaveBeenCalledTimes(1);
  });

  it("reads only after the native prompt subscription succeeds", async () => {
    let ready!: () => void;
    subscribe.mockImplementation(
      () => new Promise<void>((resolve) => (ready = resolve)),
    );
    pending.mockResolvedValue({ requests: [] });
    show();
    await waitFor(() => expect(subscribe).toHaveBeenCalledTimes(1));
    await act(async () => announceQueueChange());
    expect(pending).not.toHaveBeenCalled();
    await act(async () => ready());
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
  });

  it("never registers a listener that resolves after unmount", async () => {
    registration.autoResolve = false;
    const view = show();
    await waitFor(() => expect(windowListeners.size).toBe(1));
    view.unmount();
    await act(async () => registration.resolve!(unlisten));
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(subscribe).not.toHaveBeenCalled();
    expect(unsubscribe).not.toHaveBeenCalled();
    expect(pending).not.toHaveBeenCalled();
  });

  it("waits for a pending registration before removing it during cleanup", async () => {
    let ready!: () => void;
    subscribe.mockImplementation(
      () => new Promise<void>((resolve) => (ready = resolve)),
    );
    const view = show();
    await waitFor(() => expect(subscribe).toHaveBeenCalledTimes(1));
    const token = subscribe.mock.calls[0]![0];
    view.unmount();
    expect(unsubscribe).not.toHaveBeenCalled();
    await act(async () => ready());
    expect(unsubscribe).toHaveBeenCalledExactlyOnceWith(token);
    expect(pending).not.toHaveBeenCalled();
  });

  it("keeps a newer prompt registered when an earlier registration finishes late", async () => {
    let ready!: () => void;
    subscribe.mockImplementationOnce(
      () => new Promise<void>((resolve) => (ready = resolve)),
    );
    pending.mockResolvedValue({ requests: [] });
    const earlier = show();
    await waitFor(() => expect(subscribe).toHaveBeenCalledTimes(1));
    const firstToken = subscribe.mock.calls[0]![0];
    earlier.unmount();
    const current = show();
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    const nextToken = subscribe.mock.calls[1]![0];
    expect(nextToken).not.toBe(firstToken);
    await act(async () => ready());
    expect(unsubscribe.mock.calls).toEqual([[firstToken]]);
    current.unmount();
    await waitFor(() =>
      expect(unsubscribe.mock.calls).toEqual([[firstToken], [nextToken]]),
    );
  });

  it("does not read or unregister when native registration fails", async () => {
    subscribe.mockRejectedValue(new Error("prompt registration failed"));
    pending.mockResolvedValue({ requests: [] });
    const view = show();
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("prompt registration failed"),
    );
    await act(async () => announceQueueChange());
    await act(async () => view.unmount());
    expect(pending).not.toHaveBeenCalled();
    expect(unsubscribe).not.toHaveBeenCalled();
  });

  it("handles a registration rejected after unmount without unregistering", async () => {
    let reject!: (error: Error) => void;
    subscribe.mockImplementation(
      () => new Promise<void>((_resolve, fail) => (reject = fail)),
    );
    const view = show();
    await waitFor(() => expect(subscribe).toHaveBeenCalledTimes(1));
    view.unmount();
    await act(async () => reject(new Error("prompt registration failed")));
    expect(unsubscribe).not.toHaveBeenCalled();
    expect(pending).not.toHaveBeenCalled();
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("reports a failed unregister without leaving an unhandled rejection", async () => {
    unsubscribe.mockRejectedValue(new Error("prompt removal failed"));
    pending.mockResolvedValue({ requests: [] });
    const view = show();
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    view.unmount();
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("prompt removal failed"),
    );
  });

  it("never advertises a native prompt outside the desktop shell", async () => {
    environment.native = false;
    pending.mockResolvedValue({ requests: [] });
    const view = show();
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    view.unmount();
    expect(subscribe).not.toHaveBeenCalled();
    expect(unsubscribe).not.toHaveBeenCalled();
    expect(windowListeners.size).toBe(0);
  });

  it("does not read or register when the listener cannot be registered", async () => {
    pending.mockResolvedValue({ requests: [] });
    registration.autoResolve = false;
    show();
    await waitFor(() => expect(windowListeners.size).toBe(1));
    await act(async () => registration.reject!(new Error("no window")));
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith("no window"));
    expect(pending).not.toHaveBeenCalled();
    expect(subscribe).not.toHaveBeenCalled();
    expect(unsubscribe).not.toHaveBeenCalled();
  });

  it("re-reads when the bridge announces during a read that is still out", async () => {
    let release!: (value: { requests: (typeof question)[] }) => void;
    let reads = 0;
    pending.mockImplementation(async () => {
      reads += 1;
      if (reads === 1) return new Promise((resolve) => (release = resolve));
      return { requests: [question] };
    });
    show();
    await waitFor(() => expect(reads).toBe(1));
    await act(async () => announceQueueChange());
    expect(reads).toBe(1);
    await act(async () => release({ requests: [] }));
    await waitFor(() => expect(reads).toBe(2));
    expect(await screen.findByText(question.prompt)).toBeTruthy();
  });

  it("re-reads when the bridge announces while an answer is out", async () => {
    pending.mockResolvedValueOnce({ requests: [question] });
    pending.mockResolvedValue({ requests: [] });
    let release!: () => void;
    decide.mockImplementation(
      () => new Promise<void>((resolve) => (release = resolve)),
    );
    show();
    await screen.findByText(question.prompt);
    fireEvent.click(screen.getByText("Allow once"));
    await act(async () => announceQueueChange());
    expect(pending).toHaveBeenCalledTimes(1);
    await act(async () => release());
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(2));
  });

  it("reads the next question after answering without waiting for another event", async () => {
    const next = { ...question, id: "q2", prompt: "Allow the next plugin request?" };
    pending.mockResolvedValueOnce({ requests: [question, next] });
    pending.mockResolvedValue({ requests: [next] });
    show();
    await screen.findByText(question.prompt);
    fireEvent.click(screen.getByText("Allow once"));
    expect(await screen.findByText(next.prompt)).toBeTruthy();
    expect(pending).toHaveBeenCalledTimes(2);
  });

  it("sends only one answer while a decision is in flight", async () => {
    pending.mockResolvedValueOnce({ requests: [question] });
    pending.mockResolvedValue({ requests: [] });
    let release!: () => void;
    decide.mockImplementation(
      () => new Promise<void>((resolve) => (release = resolve)),
    );
    show();
    await screen.findByText(question.prompt);
    fireEvent.click(screen.getByText("Allow once"));
    fireEvent.click(screen.getByText("Deny"));
    expect(decide).toHaveBeenCalledTimes(1);
    expect(decide).toHaveBeenCalledWith("q1", "once");
    await act(async () => release());
    await waitFor(() => expect(screen.queryByText(question.prompt)).toBeNull());
  });

  it("does not repeat a dirty read after the prompt unmounts", async () => {
    let release!: (value: { requests: (typeof question)[] }) => void;
    pending.mockImplementation(() => new Promise((resolve) => (release = resolve)));
    const view = show();
    await waitFor(() => expect(pending).toHaveBeenCalledTimes(1));
    await act(async () => announceQueueChange());
    view.unmount();
    await act(async () => release({ requests: [question] }));
    expect(pending).toHaveBeenCalledTimes(1);
  });
});
