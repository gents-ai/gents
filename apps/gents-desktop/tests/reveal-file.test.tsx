import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

import { ToolBody } from "../src/ui/screens/tool-views";
import { node, renderIn, testApp } from "./app-fixture";

vi.mock("../src/lib/shellPlatform", async (original) => ({
  ...(await original<typeof import("../src/lib/shellPlatform")>()),
  revealInFolderLabel: () => "Reveal in Finder",
}));

const edited: RenderedToolCallView = {
  itemKey: "tool-1",
  toolName: "edit_file",
  status: "completed",
  statusKind: "success",
  presentation: {
    kind: "fileEdit",
    operation: "edit_file",
    path: "/Users/me/project/src/lib.rs",
    created: false,
    replacementsApplied: 1,
    diff: [],
    fallbackOutput: null,
  },
  reconstruction: { state: "ready" },
} as unknown as RenderedToolCallView;

/* the runtime is a user service the desktop pairs with by enrollment: only
   the home's DID says it is this machine's */
function appOn(agentDid: string) {
  return testApp({
    snapshot: {
      bootstrap: { initAgentDid: "did:key:home" },
      client: { deployments: [node({ agentDid, source: "enrollment" })] },
    },
  });
}

describe("revealing an edited file", () => {
  it("is offered for a path on the node this machine runs, paired by enrollment", () => {
    renderIn(appOn("did:key:home"), <ToolBody tool={edited} />);
    expect(
      screen.getByRole("button", { name: "Reveal in Finder" }),
    ).toBeInTheDocument();
  });

  it("is not offered for a path on a remote node", () => {
    renderIn(appOn("did:key:remote"), <ToolBody tool={edited} />);
    expect(screen.queryByRole("button", { name: "Reveal in Finder" })).toBeNull();
  });
});
