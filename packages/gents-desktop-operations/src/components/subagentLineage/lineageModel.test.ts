import { describe, expect, it } from "vitest";

import {
  buildTree,
  flattenTreeOrder,
  nodeId,
  splitNodeId,
  subtreeHasSurvivor,
} from "./lineageModel.js";
import type { SubagentTreeView } from "@source-inc/gents-desktop-client";

function tree(): SubagentTreeView {
  return {
    rootRequestId: "req-root",
    truncated: false,
    nodes: [
      {
        requestId: "req-root",
        agentDid: "did:root",
        lifecycleState: "completed",
        subagentDepth: 0,
      },
      {
        requestId: "req-child",
        agentDid: "did:child",
        lifecycleState: "processing",
        subagentDepth: 1,
      },
    ],
    edges: [
      {
        parentRequestId: "req-root",
        childRequestId: "req-child",
        parentToolCallId: "tool-1",
        awaitMode: "background",
        cancelPolicy: "cascade",
      },
    ],
  };
}

describe("lineageModel", () => {
  it("builds request and tool nodes with sorted deployment ids", () => {
    const { root, deployments } = buildTree(tree());

    expect(root?.id).toBe("req-root");
    expect(root?.children[0]?.id).toBe("tool-1");
    expect(root?.children[0]?.child?.id).toBe("req-child");
    expect(deployments).toEqual(["did:child", "did:root"]);
  });

  it("flattens only expanded branches", () => {
    const { root } = buildTree(tree());

    expect(flattenTreeOrder(root, new Set()).map(nodeId)).toEqual([
      "req:req-root",
    ]);
    expect(
      flattenTreeOrder(root, new Set(["req:req-root"])).map(nodeId),
    ).toEqual(["req:req-root", "tool:tool-1"]);
  });

  it("keeps terminal parents visible when a live descendant survives", () => {
    const { root } = buildTree(tree());

    expect(root).not.toBeNull();
    expect(
      subtreeHasSurvivor(root!, 0, {
        depth: "all",
        deployments: new Set(),
        liveOnly: true,
      }),
    ).toBe(true);
  });

  it("splits encoded node ids", () => {
    expect(splitNodeId("tool:tool-1")).toEqual(["tool", "tool-1"]);
    expect(splitNodeId("req-root")).toEqual(["req", "req-root"]);
  });
});
