import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  backgroundName,
  backgroundRunLabel,
  backgroundText,
  withoutRoutingHeader,
} from "@/screens/TranscriptInputs";
import type { BackgroundResult } from "@/screens/transcript-groups";

describe("a message's routing line", () => {
  const header =
    "[message agent:default session:5c1d node:did:key:z6Mk · at:2026-11-01T13:53:39Z · cause:9a7e]";

  it("is left out of the text a person reads", () => {
    expect(withoutRoutingHeader(`${header}\n\nthe build is green\n\nnext step`)).toBe(
      "the build is green\n\nnext step",
    );
    expect(withoutRoutingHeader(`${header}\r\n\r\nhello`)).toBe("hello");
    expect(withoutRoutingHeader(header)).toBe("");
  });

  it("is only a first line shaped like one", () => {
    for (const text of [
      "the build is green",
      "[message agent:default but no closing bracket\n\nbody",
      "[note] something else\n\nbody",
      `intro\n${header}\n\nbody`,
      ` ${header}\n\nbody`,
    ])
      expect(withoutRoutingHeader(text)).toBe(text);
  });
});

describe("naming a background result", () => {
  const completion = (status: string, tool = "bash_unrestricted") =>
    `<tool-completion tool_call_id="call-1" tool_name="${tool}" status="${status}">\n  <result>ok</result>\n</tool-completion>`;
  const command = (text: string) =>
    ({
      itemKey: "t",
      toolName: "bash_unrestricted",
      statusKind: "success",
      presentation: { kind: "command", command: text },
    }) as unknown as RenderedToolCallView;

  it("says what the call ran when it is drawn, and how it ended", () => {
    expect(
      backgroundText(
        backgroundName(completion("completed"), command("cd crates && cargo test")),
      ),
    ).toBe("Background cargo test finished");
    expect(backgroundText(backgroundName(completion("failed"), null))).toBe(
      "Background bash_unrestricted failed",
    );
    expect(backgroundText(backgroundName(completion("cancelled"), null))).toBe(
      "Background bash_unrestricted cancelled",
    );
  });

  it("says only that a result came when the input names no call", () => {
    expect(
      backgroundName("Review the new background completion results.", null),
    ).toBeNull();
    expect(backgroundText(null)).toBe("Background result");
  });

  it("counts the results a run folds, past inputs that name no call", () => {
    const result = (content: string): BackgroundResult =>
      ({ item: { content }, call: null }) as unknown as BackgroundResult;
    const wake = result("Review the new background completion results.");
    expect(backgroundRunLabel([result(completion("completed")), wake])).toBe(
      "Background bash_unrestricted finished",
    );
    expect(
      backgroundRunLabel([
        result(completion("completed")),
        result(completion("failed")),
        result(completion("completed")),
        wake,
      ]),
    ).toBe("3 background results");
    expect(backgroundRunLabel([wake, wake])).toBe("2 background results");
  });
});
