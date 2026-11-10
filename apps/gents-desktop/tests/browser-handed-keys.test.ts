import { describe, expect, it } from "vitest";
import { keyEventOf } from "../src/ui/screens/browser/handedKeys";

describe("a command key handed back from the browser page", () => {
  it("replays a letter with its code and modifiers", () => {
    expect(
      keyEventOf({
        key: "k",
        keyCode: 0x28,
        meta: true,
        shift: false,
        alt: false,
        ctrl: false,
      }),
    ).toMatchObject({
      key: "k",
      code: "KeyK",
      metaKey: true,
      shiftKey: false,
      bubbles: true,
    });
  });

  it("names the keys AppKit reports as private-use characters", () => {
    expect(
      keyEventOf({
        key: "",
        keyCode: 0x7b,
        meta: true,
        shift: true,
        alt: false,
        ctrl: false,
      }),
    ).toMatchObject({ key: "ArrowLeft", code: "ArrowLeft", shiftKey: true });
  });

  it("keeps the digit row's codes in their own order", () => {
    expect(
      keyEventOf({
        key: "5",
        keyCode: 0x17,
        meta: true,
        shift: false,
        alt: false,
        ctrl: false,
      }).code,
    ).toBe("Digit5");
    expect(
      keyEventOf({
        key: "6",
        keyCode: 0x16,
        meta: true,
        shift: false,
        alt: false,
        ctrl: false,
      }).code,
    ).toBe("Digit6");
  });
});
