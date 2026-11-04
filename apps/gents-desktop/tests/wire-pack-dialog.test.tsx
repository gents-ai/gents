import { act, fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  refusedWhileRunning,
  WirePackDialog,
} from "../src/ui/screens/agent/WirePackDialog";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const REFUSAL =
  "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents pack wire demo/hello_plugin --agent did:key:z6Mk:default --home <node-home>` without --graphql";

describe("enabling a pack's plugin for an agent", () => {
  it("reads the running node's refusal for its command", () => {
    expect(refusedWhileRunning(REFUSAL)).toBe(
      "gents pack wire demo/hello_plugin --agent did:key:z6Mk:default --home <node-home>",
    );
    expect(refusedWhileRunning("offline")).toBeUndefined();
  });

  it("says in words why it can't enable while the node runs, with the command to run", async () => {
    const wirePack = vi.fn(async ({ preview }: { preview: boolean }) => {
      if (preview)
        return {
          report: { job: null, next_call: null, warnings: [] },
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
        targetsFrom={[]}
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
    expect(screen.getByText(/gents pack wire demo\/hello_plugin/)).toBeInTheDocument();
    expect(screen.queryByText(/through GraphQL/)).toBeNull();
  });
});
