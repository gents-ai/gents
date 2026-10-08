import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

import { activateUxPlugin, isUxPluginLive } from "@/contrib/activate";
import { installUxHost, type GentsUxPlugin, type UxHost } from "@/contrib/plugin";
import {
  resetUxPluginStore,
  setUxPluginEnabled,
  uxDecisions,
  uxPluginActive,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { Slot } from "@/contrib/react/slot";
import { registry } from "@/contrib/registry";
import { SESSION_HEADER_ACTIONS_AREA } from "@/contrib/types";

const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: () => {},
    deployments: () => [],
    selectedDeployment: () => null,
    selectedSessionId: () => null,
    send: async () => {},
  },
  bridge: async () => ({}) as never,
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => null,
  },
};

beforeEach(() => {
  installUxHost(host);
  resetUxPluginStore();
  /* jsdom's storage is not usable under this origin; the decisions store
     tolerates that, but the persistence assertions need a real one */
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
    removeItem: (key: string) => store.delete(key),
  });
});
afterEach(() => {
  resetUxPluginStore();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const chipPlugin = (id: string, extra: Partial<GentsUxPlugin> = {}): GentsUxPlugin => ({
  id,
  name: id,
  register(ctx) {
    ctx.register({
      id: "chip",
      area: SESSION_HEADER_ACTIONS_AREA,
      render: () => <span data-testid={`chip-${id}`}>{id}</span>,
    });
  },
  ...extra,
});

describe("activation and the plugin store", () => {
  it("activates by default, and a persisted decision survives a re-activation pass", async () => {
    activateUxPlugin(chipPlugin("a"), { door: "bundled" });
    expect(uxPluginRecords().a?.status).toBe("loaded");
    expect(isUxPluginLive("a")).toBe(true);

    await setUxPluginEnabled("a", false);
    expect(uxPluginRecords().a?.status).toBe("disabled");
    expect(isUxPluginLive("a")).toBe(false);
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(0);
    expect(uxDecisions().a).toBe(false);
    expect(window.localStorage.getItem("gents.ux.decisions.v1")).toContain('"a":false');

    activateUxPlugin(chipPlugin("a"), { door: "bundled" });
    expect(uxPluginRecords().a?.status).toBe("disabled");

    await setUxPluginEnabled("a", true);
    expect(uxPluginRecords().a?.status).toBe("loaded");
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(1);
  });

  it("ships an opt-in plugin off until the user flips it", () => {
    activateUxPlugin(chipPlugin("opt", { defaultEnabled: false }), { door: "bundled" });
    expect(uxPluginRecords().opt?.status).toBe("disabled");
    expect(uxPluginActive("opt", false)).toBe(false);
    expect(uxPluginActive("opt", true)).toBe(true);
  });

  it("rolls back a register() that throws and reports on the plugin's row", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    activateUxPlugin(
      {
        id: "bad",
        register(ctx) {
          ctx.register({
            id: "c",
            area: SESSION_HEADER_ACTIONS_AREA,
            render: () => null,
          });
          throw new Error("half way");
        },
      },
      { door: "bundled" },
    );
    expect(uxPluginRecords().bad?.status).toBe("error");
    expect(uxPluginRecords().bad?.error).toBe("half way");
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(0);
  });

  it("reports an async register() rejection on the row", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    activateUxPlugin(
      {
        id: "async",
        async register() {
          await Promise.resolve();
          throw new Error("later");
        },
      },
      { door: "bundled" },
    );
    expect(uxPluginRecords().async?.status).toBe("loaded");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(uxPluginRecords().async?.status).toBe("error");
    expect(uxPluginRecords().async?.error).toBe("later");
  });
});

describe("<Slot>", () => {
  it("renders live contributions and re-renders on a toggle", async () => {
    activateUxPlugin(chipPlugin("s1"), { door: "bundled" });
    render(<Slot area={SESSION_HEADER_ACTIONS_AREA} />);
    expect(screen.getByTestId("chip-s1")).toBeInTheDocument();
    await act(() => setUxPluginEnabled("s1", false));
    expect(screen.queryByTestId("chip-s1")).toBeNull();
    await act(() => setUxPluginEnabled("s1", true));
    expect(screen.getByTestId("chip-s1")).toBeInTheDocument();
  });

  it("contains one throwing contribution without taking the others down", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    activateUxPlugin(chipPlugin("ok"), { door: "bundled" });
    activateUxPlugin(
      {
        id: "boom",
        register(ctx) {
          ctx.register({
            id: "chip",
            area: SESSION_HEADER_ACTIONS_AREA,
            render: () => {
              throw new Error("render failed");
            },
          });
        },
      },
      { door: "bundled" },
    );
    render(<Slot area={SESSION_HEADER_ACTIONS_AREA} />);
    expect(screen.getByTestId("chip-ok")).toBeInTheDocument();
    expect(screen.getByTestId("contrib-error-chip")).toHaveAttribute(
      "title",
      "boom:chip: render failed",
    );
  });
});
