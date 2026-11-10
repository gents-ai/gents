import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  CompactionEditor,
  compactionInWords,
  newCompactionDocument,
} from "../src/ui/screens/agent/CompactionEditor";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const api = () => ({
  applyConfigComponents: vi.fn().mockResolvedValue({}),
  patchConfigComponents: vi.fn().mockResolvedValue({}),
});

const standard = {
  compaction_id: "compaction-standard",
  node_did: deployment.nodeDid,
  display_name: "Standard (50%)",
  threshold: 0.5,
  keep_recent_tokens: 12_000,
  inference_profile_id: "profile-a",
};

describe("a compaction", () => {
  it("says what it does in a line, the runtime default included", () => {
    expect(compactionInWords(deployment, undefined)).toBe(
      "At 75% of the context window, trims old tool results and summarizes the rest, keeping the last 20k tokens.",
    );
    expect(compactionInWords(deployment, standard)).toMatch(
      /^At 50% of the context window, trims old tool results and summarizes the rest, written by .+, keeping the last 12k tokens\.$/,
    );
    expect(
      compactionInWords(deployment, { ...standard, strategy: "StripToolResults" }),
    ).toBe(
      "At 50% of the context window, trims old tool results, keeping the last 12k tokens.",
    );
  });

  it("creates one from the sheet, its threshold a percent", async () => {
    const calls = api();
    const onSaved = vi.fn();
    const fresh = newCompactionDocument(deployment);
    renderIn(
      testApp({ api: calls }),
      <CompactionEditor
        deployment={deployment}
        compaction={fresh}
        draft={{ onSaved, onCancel: vi.fn() }}
      />,
    );
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Name"), "Tight");
    await user.type(screen.getByLabelText("Threshold"), "60");
    await user.click(screen.getByRole("button", { name: "Create" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(fresh.compaction_id));
    expect(calls.applyConfigComponents).toHaveBeenCalledWith({
      document: {
        node: { node_did: deployment.nodeDid },
        compactions: [
          expect.objectContaining({
            compaction_id: fresh.compaction_id,
            display_name: "Tight",
            threshold: 0.6,
            keep_recent_tokens: null,
          }),
        ],
      },
    });
  });

  it("patches only the compaction it edits, and refuses what the runtime would", async () => {
    const calls = api();
    renderIn(
      testApp({ api: calls }),
      <CompactionEditor
        deployment={{ ...deployment, compactions: [standard] }}
        compaction={standard}
      />,
    );
    const user = userEvent.setup();
    const threshold = screen.getByLabelText("Threshold");
    await user.clear(threshold);
    await user.type(threshold, "150");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Threshold must be 100 or less",
    );
    expect(calls.patchConfigComponents).not.toHaveBeenCalled();

    await user.clear(threshold);
    await user.type(threshold, "40");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(calls.patchConfigComponents).toHaveBeenCalledWith({
        nodeDid: deployment.nodeDid,
        patches: [
          {
            collection: "Compaction",
            id: "compaction-standard",
            changes: expect.objectContaining({
              threshold: 0.4,
              keep_recent_tokens: 12_000,
              inference_profile_id: "profile-a",
            }),
          },
        ],
      }),
    );
  });
});
