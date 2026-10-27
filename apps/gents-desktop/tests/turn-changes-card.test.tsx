import { fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ChangedFile } from "@/lib/turnChanges";
import { resetChangesView, useShownChanges } from "@/screens/changes";
import { TurnChanges } from "@/screens/TurnChanges";
import type { Route } from "@/lib/router";
import { renderRouted, testApp } from "./app-fixture";

const SCOPE = { nodeDid: "did:node", sessionId: "s" };
const file = (n: number): ChangedFile => ({
  path: `/p/src/f${n}.rs`,
  added: 1,
  removed: 0,
  edits: [
    {
      path: `/p/src/f${n}.rs`,
      requestId: "r",
      toolCallDocId: `c${n}`,
      added: 1,
      removed: 0,
    },
  ],
});

function Shown() {
  const shown = useShownChanges(SCOPE.sessionId);
  return <output>{shown?.path ?? "none"}</output>;
}

describe("a turn's change card", () => {
  afterEach(resetChangesView);

  it("lists a few files and opens the panel for the rest", () => {
    const files = Array.from({ length: 7 }, (_, i) => file(i));
    renderRouted(testApp(), { name: "sessions" } as Route, () => (
      <>
        <TurnChanges scope={SCOPE} turn="t" files={files} />
        <Shown />
      </>
    ));
    expect(screen.getAllByRole("listitem")).toHaveLength(5);
    fireEvent.click(screen.getByRole("button", { name: "2 more files" }));
    expect(screen.getByRole("status")).toHaveTextContent("/p/src/f5.rs");
  });
});
