import { describe, expect, it } from "vitest";

import { distinctNames, pathName, shortPath } from "@/lib/paths";

describe("shortPath", () => {
  describe("as a tool row keeps its last three segments", () => {
    const row = (path: string) => shortPath(path, { keep: 3 });

    it("keeps a relative path whole", () => {
      expect(row("src/api/export.rs")).toBe("src/api/export.rs");
    });

    it("shortens an absolute POSIX path to its last segments", () => {
      expect(row("/home/dev/work/src/api/export.rs")).toBe("…/src/api/export.rs");
    });

    /* a Windows path is not a POSIX path with odd separators: splitting on
       the wrong one produced one segment and left the whole path in the row */
    it("shortens a Windows path with its own separator", () => {
      expect(row("C:\\Users\\dev\\src\\api\\export.rs")).toBe("…\\src\\api\\export.rs");
    });

    it("leaves a path already short enough", () => {
      expect(row("/etc/hosts")).toBe("/etc/hosts");
    });
  });

  it("starts a path under this computer's home at ~", () => {
    expect(shortPath("/Users/main/work/notes", { home: "/Users/main" })).toBe(
      "~/work/notes",
    );
    expect(shortPath("/Users/main", { home: "/Users/main/" })).toBe("~");
    expect(shortPath("/Users/mainly/notes", { home: "/Users/main" })).toBe(
      "/Users/mainly/notes",
    );
    expect(shortPath("C:\\Users\\Dev\\docs", { home: "c:\\users\\dev" })).toBe(
      "~\\docs",
    );
  });

  it("drops folders from the left to fit, never the name", () => {
    const path = "/Users/main/Projects/gents/gents-test-workspace";
    expect(shortPath(path, { home: "/Users/main", max: 34 })).toBe(
      "~/…/gents/gents-test-workspace",
    );
    expect(shortPath(path, { max: 20 })).toBe("…/gents-test-workspace");
    expect(shortPath(path, { home: "/Users/main", max: 80 })).toBe(
      "~/Projects/gents/gents-test-workspace",
    );
  });
});

describe("distinctNames", () => {
  it("adds where a folder is only when two share its name", () => {
    const names = distinctNames(
      ["/Users/main/work/notes", "/Users/main/home/notes", "/Users/main/site"],
      "/Users/main",
    );
    expect(names.get("/Users/main/work/notes")).toEqual({
      name: "notes",
      where: "~/work",
    });
    expect(names.get("/Users/main/home/notes")).toEqual({
      name: "notes",
      where: "~/home",
    });
    expect(names.get("/Users/main/site")).toEqual({ name: "site", where: null });
  });

  it("names a path by its last segment", () => {
    expect(pathName("/home/u/notes/")).toBe("notes");
    expect(pathName("C:\\Users\\u\\docs")).toBe("docs");
  });
});
