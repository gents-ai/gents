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
  whole: true,
});

function Shown({ nodeDid }: { nodeDid: string }) {
  const shown = useShownChanges({ nodeDid, sessionId: SCOPE.sessionId });
  return <output data-testid={`shown-${nodeDid}`}>{shown?.path ?? "none"}</output>;
}

describe("a turn's change card", () => {
  afterEach(resetChangesView);

  it("lists a few files and opens the panel for the rest", () => {
    const files = Array.from({ length: 7 }, (_, i) => file(i));
    renderRouted(testApp(), { name: "sessions" } as Route, () => (
      <>
        <TurnChanges scope={SCOPE} turn="t" files={files} />
        <Shown nodeDid={SCOPE.nodeDid} />
      </>
    ));
    expect(screen.getAllByRole("listitem")).toHaveLength(5);
    fireEvent.click(screen.getByRole("button", { name: "2 more files" }));
    expect(screen.getByRole("status")).toHaveTextContent("/p/src/f5.rs");
  });

  it("counts lines only where they are a file's net change", () => {
    const twice: ChangedFile = {
      ...file(2),
      added: 4,
      edits: [file(2).edits[0]!, { ...file(2).edits[0]!, toolCallDocId: "c2b" }],
    };
    renderRouted(testApp(), { name: "sessions" } as Route, () => (
      <TurnChanges scope={SCOPE} turn="t" files={[file(1), twice]} />
    ));
    const card = screen.getByTestId("turn-changes");
    expect(screen.getByRole("button", { name: /f1\.rs/ })).toHaveTextContent("+1 −0");
    expect(screen.getByRole("button", { name: /f2\.rs/ })).toHaveTextContent("2 edits");
    expect(card.querySelector("header")).not.toHaveTextContent("+");
  });

  it("marks a turn begun before the loaded rows as partial", () => {
    const files = [{ ...file(1), whole: false }];
    renderRouted(testApp(), { name: "sessions" } as Route, () => (
      <TurnChanges scope={SCOPE} turn="t" files={files} />
    ));
    expect(
      screen.getByTestId("turn-changes").querySelector("header"),
    ).toHaveTextContent("Partial");
    expect(screen.getByRole("button", { name: /f1\.rs/ })).not.toHaveTextContent("+1");
  });

  it("keeps equal session and turn IDs scoped to their node", () => {
    const otherScope = { nodeDid: "did:other", sessionId: SCOPE.sessionId };
    renderRouted(testApp(), { name: "sessions" } as Route, () => (
      <>
        <TurnChanges scope={SCOPE} turn="same-turn" files={[file(1)]} />
        <TurnChanges scope={otherScope} turn="same-turn" files={[file(2)]} />
        <Shown nodeDid={SCOPE.nodeDid} />
        <Shown nodeDid={otherScope.nodeDid} />
      </>
    ));

    const rows = screen.getAllByRole("button", { name: /f[12]\.rs/ });
    fireEvent.click(rows[0]!);

    expect(screen.getByTestId(`shown-${SCOPE.nodeDid}`)).toHaveTextContent(
      "/p/src/f1.rs",
    );
    expect(screen.getByTestId(`shown-${otherScope.nodeDid}`)).toHaveTextContent("none");
    expect(
      screen
        .getAllByRole("button", { name: /f[12]\.rs/ })
        .map((row) => row.getAttribute("aria-current")),
    ).toEqual(["true", null]);
  });
});
