import { beforeEach, describe, expect, it } from "vitest";
import { resetViewedFiles, viewedFiles } from "../src/ui/app/viewedFiles";
import type { ChangedFile } from "../src/ui/lib/turnChanges";

const scope = { nodeDid: "did:key:z6MkNode", sessionId: "s1" };
const edit = (toolCallDocId: string) => ({
  path: "src/lexer.rs",
  requestId: "r1",
  toolCallDocId,
  added: 1,
  removed: 1,
});
const file = (...edits: string[]): ChangedFile => ({
  path: "src/lexer.rs",
  added: 1,
  removed: 1,
  edits: edits.map(edit),
});

describe("viewed files", () => {
  beforeEach(resetViewedFiles);

  it("keeps a file viewed at the version it was viewed, and not after a later edit", () => {
    viewedFiles.set(scope, file("e1"), true);
    expect(viewedFiles.isViewed(scope, file("e1"))).toBe(true);
    expect(viewedFiles.isViewed(scope, file("e1", "e2"))).toBe(false);
    expect(viewedFiles.isViewed({ ...scope, sessionId: "s2" }, file("e1"))).toBe(false);
  });

  it("keeps viewed versions independent when reviewing older turns", () => {
    viewedFiles.set(scope, file("e2"), true);
    viewedFiles.set(scope, file("e1"), true);
    expect(viewedFiles.isViewed(scope, file("e2"))).toBe(true);
    expect(viewedFiles.isViewed(scope, file("e1"))).toBe(true);

    viewedFiles.set(scope, file("e1"), false);
    expect(viewedFiles.isViewed(scope, file("e1"))).toBe(false);
    expect(viewedFiles.isViewed(scope, file("e2"))).toBe(true);
  });

  it("forgets a file marked not viewed", () => {
    viewedFiles.set(scope, file("e1"), true);
    viewedFiles.set(scope, file("e1"), false);
    expect(viewedFiles.isViewed(scope, file("e1"))).toBe(false);
  });
});
