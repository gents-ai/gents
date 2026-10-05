/* The words and paths a row is named by. */
import { describe, expect, it } from "vitest";
import { shortPath } from "@/screens/tool-summary";

describe("shortPath", () => {
  it("keeps a relative path whole", () => {
    expect(shortPath("src/api/export.rs")).toBe("src/api/export.rs");
  });

  it("shortens an absolute POSIX path to its last segments", () => {
    expect(shortPath("/home/dev/work/src/api/export.rs")).toBe("…/src/api/export.rs");
  });

  /* a Windows path is not a POSIX path with odd separators: splitting on
     the wrong one produced one segment and left the whole path in the row */
  it("shortens a Windows path with its own separator", () => {
    expect(shortPath("C:\\Users\\dev\\src\\api\\export.rs")).toBe(
      "…\\src\\api\\export.rs",
    );
  });

  it("leaves a path already short enough", () => {
    expect(shortPath("/etc/hosts")).toBe("/etc/hosts");
  });
});
