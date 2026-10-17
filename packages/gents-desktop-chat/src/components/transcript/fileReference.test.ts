import { expect, it } from "vitest";
import { fileReference } from "./fileReference.js";

it("preserves file paths and extracts one-based line references", () => {
  expect(fileReference("src/main.rs#L42-L50")).toEqual({
    path: "src/main.rs",
    line: 42,
  });
  expect(fileReference("/srv/My%20Project/main.rs:12:3")).toEqual({
    path: "/srv/My Project/main.rs",
    line: 12,
  });
  expect(fileReference("src/main.rs")).toEqual({
    path: "src/main.rs",
    line: undefined,
  });
});

it("leaves external links and page anchors to the host's ordinary link handler", () => {
  for (const value of [
    "https://example.com/file",
    "mailto:dev@example.com",
    "command:run",
    "//host/file",
    "#heading",
    "file?query=1",
    "file#L0",
    "%broken",
    undefined,
  ])
    expect(fileReference(value)).toBeNull();
});
