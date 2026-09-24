import { describe, expect, it } from "vitest";
import { initials } from "../src/ui/screens/behavior";
import { spoken } from "../src/ui/screens/tool-summary";

describe("behavior initials", () => {
  it("skips a leading article even for a one-word name after it", () => {
    expect(initials("The Engineer")).toBe("En");
    expect(initials("An Ops Lead")).toBe("Ol");
    expect(initials("Reviewer")).toBe("Re");
    expect(initials("The")).toBe("Th");
  });
});

describe("spoken command", () => {
  it("keeps a command that has leading environment assignments", () => {
    expect(spoken("FOO=1 cargo test")).toBe("cargo test");
    expect(spoken('RUST_LOG="debug x" BAR=2 cargo run')).toBe("cargo run");
  });

  it("drops cd and pure assignments between steps", () => {
    expect(spoken("cd crates/gents && FOO=1 && cargo test -p gents")).toBe(
      "cargo test -p gents",
    );
    expect(spoken("cd /tmp")).toBe("cd /tmp");
  });
});
