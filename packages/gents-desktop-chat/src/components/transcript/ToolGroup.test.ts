import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ToolGroup } from "./ToolGroup.js";

const tool = (state: RenderedToolCallView["reconstruction"]["state"]): RenderedToolCallView => ({
  itemKey: "tool-physical",
  toolName: "read_file",
  statusKind: "success",
  presentation: {
    kind: "generic",
    summary: null,
    input: "stale input",
    output: "stale partial result",
  },
  reconstruction: { state },
});

describe("canonical tool payload availability", () => {
  it.each(["loading", "denied", "invalid"] as const)(
    "shows %s without presenting a partial result",
    (state) => {
      const html = renderToStaticMarkup(createElement(ToolGroup, { tools: [tool(state)] }));
      expect(html).toContain(`data-testid="tool-output-${state}-tool-physical"`);
      expect(html).not.toContain("stale input");
      expect(html).not.toContain("stale partial result");
      expect(html).not.toContain("tool-payload-copy");
      expect(html).toContain("completed");
    },
  );

  it("keeps an empty ready result distinct from loading", () => {
    const html = renderToStaticMarkup(createElement(ToolGroup, {
      tools: [{ ...tool("ready"), presentation: { kind: "generic", summary: null, input: null, output: "" } }],
    }));
    expect(html).not.toContain("tool-output-loading");
    expect(html).toContain("completed");
  });
});
