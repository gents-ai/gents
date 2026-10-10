import { fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

import { revealInFolder } from "../src/lib/nativeShell";

import { ToolBody } from "../src/ui/screens/tool-views";
import { node, renderIn, testApp } from "./app-fixture";

vi.mock("../src/lib/shellPlatform", async (original) => ({
  ...(await original<typeof import("../src/lib/shellPlatform")>()),
  revealInFolderLabel: () => "Reveal in Finder",
}));

vi.mock("../src/lib/nativeShell", () => ({
  revealInFolder: vi.fn().mockResolvedValue(undefined),
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
function appOn(nodeDid: string) {
  return testApp({
    snapshot: {
      bootstrap: { initNodeDid: "did:key:home" },
      client: { deployments: [node({ nodeDid, source: "enrollment" })] },
    },
  });
}

const relative = {
  ...edited,
  presentation: {
    ...edited.presentation,
    path: "src/lib.rs",
    revealPath: "/Users/me/project/src/lib.rs",
  },
} as RenderedToolCallView;

describe("revealing an edited file", () => {
  it("reveals the runtime-resolved target of a relative path", () => {
    renderIn(appOn("did:key:home"), <ToolBody tool={relative} />);
    fireEvent.click(screen.getByRole("button", { name: "Reveal in Finder" }));
    expect(revealInFolder).toHaveBeenCalledWith("/Users/me/project/src/lib.rs");
  });

  it("does not reveal a resolved target for failed work", () => {
    renderIn(
      appOn("did:key:home"),
      <ToolBody tool={{ ...relative, statusKind: "error" }} />,
    );
    expect(screen.queryByRole("button", { name: "Reveal in Finder" })).toBeNull();
  });

  it("does not resolve a relative path on the desktop", () => {
    const tool = {
      ...relative,
      presentation: { ...relative.presentation, revealPath: null },
    } as RenderedToolCallView;
    renderIn(appOn("did:key:home"), <ToolBody tool={tool} />);
    expect(screen.queryByRole("button", { name: "Reveal in Finder" })).toBeNull();
  });
  it("is offered for a path on the node this machine runs, paired by enrollment", () => {
    renderIn(appOn("did:key:home"), <ToolBody tool={edited} />);
    expect(
      screen.getByRole("button", { name: "Reveal in Finder" }),
    ).toBeInTheDocument();
  });

  it("is not offered for a path on a remote node", () => {
    renderIn(appOn("did:key:remote"), <ToolBody tool={relative} />);
    expect(screen.queryByRole("button", { name: "Reveal in Finder" })).toBeNull();
  });
});
