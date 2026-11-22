import { act, cleanup, fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  refusedWhileRunning,
  WirePackDialog,
} from "../src/ui/screens/agent/WirePackDialog";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const REFUSAL =
  "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents config pack wire demo/hello_plugin --agent did:key:z6Mk:default --home <node-home>` without --graphql";

describe("enabling a pack's plugin for an agent", () => {
  it("reads the running node's refusal for its command", () => {
    expect(refusedWhileRunning(REFUSAL)).toBe(
      "gents config pack wire demo/hello_plugin --agent did:key:z6Mk:default --home <node-home>",
    );
    expect(refusedWhileRunning("offline")).toBeUndefined();
  });

  it("says in words why it can't enable while the node runs, with the command to run", async () => {
    const wirePack = vi.fn(async ({ preview }: { preview: boolean }) => {
      if (preview)
        return {
          report: { job: null, next_call: null, warnings: [], policy_warnings: [] },
          authority: null,
        };
      throw new Error(REFUSAL);
    });
    const app = testApp({ api: { wirePack } });
    renderIn(
      app,
      <WirePackDialog
        plugin="demo/hello_plugin"
        agents={deployment.agents}
        targetsFrom={["demo/targets"]}
        onClose={() => {}}
      />,
    );
    const enable = await screen.findByRole("button", { name: "Enable" });
    await vi.waitFor(() => expect(enable).toBeEnabled());
    await act(async () => {
      fireEvent.click(enable);
    });
    expect(
      await screen.findByText(
        /can’t be enabled from the app while this node is running/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/gents config pack wire 'demo\/hello_plugin'/),
    ).toHaveTextContent("--targets-from 'demo/targets'");
    expect(screen.queryByText(/through GraphQL/)).toBeNull();
  });

  it("allows a new dialog to try after an earlier refusal", async () => {
    const refusing = vi.fn(async ({ preview }: { preview: boolean }) => {
      if (preview)
        return {
          report: { job: null, next_call: null, warnings: [], policy_warnings: [] },
          authority: null,
        };
      throw new Error(REFUSAL);
    });
    renderIn(
      testApp({ api: { wirePack: refusing } }),
      <WirePackDialog
        plugin="demo/hello_plugin"
        agents={deployment.agents}
        targetsFrom={[]}
        onClose={() => {}}
      />,
    );
    const enable = await screen.findByRole("button", { name: "Enable" });
    await vi.waitFor(() => expect(enable).toBeEnabled());
    await act(async () => {
      fireEvent.click(enable);
    });
    cleanup();
    const wirePack = vi.fn(async () => ({
      report: { job: null, next_call: null, warnings: [], policy_warnings: [] },
      authority: null,
    }));
    renderIn(
      testApp({ api: { wirePack } }),
      <WirePackDialog
        plugin="demo/hello_plugin"
        agents={deployment.agents}
        targetsFrom={[]}
        onClose={() => {}}
      />,
    );
    expect(
      screen.queryByText(/can’t be enabled from the app while this node is running/),
    ).toBeNull();
    await vi.waitFor(() => expect(wirePack).toHaveBeenCalled());
    await vi.waitFor(() =>
      expect(screen.getByRole("button", { name: "Enable" })).toBeEnabled(),
    );
  });

  it("says a preview it cannot plan yet in words, and keeps Enable off", async () => {
    const wirePack = vi.fn(async () => ({
      skipped: true as const,
      preview: true as const,
      agent: deployment.agents[0].agentDid,
      plugin: "demo/hello_plugin",
      slot: "demo-worker",
      reason: "agent_identity_pending" as const,
      next: "install the pack, which creates the agent of slot demo-worker, then wire demo/hello_plugin",
    }));
    renderIn(
      testApp({ api: { wirePack } }),
      <WirePackDialog
        plugin="demo/hello_plugin"
        agents={deployment.agents}
        targetsFrom={[]}
        onClose={() => {}}
      />,
    );
    expect(
      await screen.findByText(/its pack creates the agent “demo-worker”/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Install the pack first/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Enable" })).toBeDisabled();
  });

  it("says the agent's own dynamic-script setting was kept and how to change it", async () => {
    const policy = (dynamic: "off" | "check" | "run", targets: string[]) => ({
      dynamic,
      dynamic_targets: targets,
      trusted_packs: [],
    });
    const wirePack = vi.fn(async () => ({
      plugin: "demo/hello_plugin",
      agent: deployment.agents[0].agentDid,
      preview: true,
      authority: null,
      report: {
        changed: [],
        job: null,
        warnings: [],
        next_call: null,
        policy_warnings: [
          {
            code: "dynamic_mode_kept" as const,
            plugin: "demo/hello_plugin",
            tools_id: "engineer-tools",
            requested: policy("check", ["wf-a"]),
            kept: policy("run", ["wf-b", "wf-a"]),
            previous_digest: null,
            installed_digest: "sha256:aa",
            remedy: "gents config tools show engineer-tools",
          },
        ],
      },
    }));
    renderIn(
      testApp({ api: { wirePack } }),
      <WirePackDialog
        plugin="demo/hello_plugin"
        agents={deployment.agents}
        targetsFrom={[]}
        onClose={() => {}}
      />,
    );
    const warning = await screen.findByTestId("wire-policy-warning");
    expect(warning).toHaveTextContent(
      "Your dynamic-script setting is kept: run for wf-b, wf-a. The pack asks for check for wf-a.",
    );
    expect(warning).toHaveTextContent("engineer-tools");
    expect(warning).not.toHaveTextContent("gents config");
    await vi.waitFor(() =>
      expect(screen.getByRole("button", { name: "Enable" })).toBeEnabled(),
    );
  });
});
