/* A command key pressed while the browser page had the keyboard comes back
   from browser.rs as macOS reported it; the app's shortcuts read keyboard
   events, so it is replayed as one, with the `key` and `code` the app's
   own window would have given it. */

export type HandedKey = {
  key: string;
  keyCode: number;
  meta: boolean;
  shift: boolean;
  alt: boolean;
  ctrl: boolean;
};

/* macOS virtual key codes (ANSI layout) to KeyboardEvent codes */
const CODES: Record<number, string> = {
  0x00: "KeyA",
  0x01: "KeyS",
  0x02: "KeyD",
  0x03: "KeyF",
  0x04: "KeyH",
  0x05: "KeyG",
  0x06: "KeyZ",
  0x07: "KeyX",
  0x08: "KeyC",
  0x09: "KeyV",
  0x0b: "KeyB",
  0x0c: "KeyQ",
  0x0d: "KeyW",
  0x0e: "KeyE",
  0x0f: "KeyR",
  0x10: "KeyY",
  0x11: "KeyT",
  0x12: "Digit1",
  0x13: "Digit2",
  0x14: "Digit3",
  0x15: "Digit4",
  0x16: "Digit6",
  0x17: "Digit5",
  0x18: "Equal",
  0x19: "Digit9",
  0x1a: "Digit7",
  0x1b: "Minus",
  0x1c: "Digit8",
  0x1d: "Digit0",
  0x1e: "BracketRight",
  0x1f: "KeyO",
  0x20: "KeyU",
  0x21: "BracketLeft",
  0x22: "KeyI",
  0x23: "KeyP",
  0x24: "Enter",
  0x25: "KeyL",
  0x26: "KeyJ",
  0x27: "Quote",
  0x28: "KeyK",
  0x29: "Semicolon",
  0x2a: "Backslash",
  0x2b: "Comma",
  0x2c: "Slash",
  0x2d: "KeyN",
  0x2e: "KeyM",
  0x2f: "Period",
  0x30: "Tab",
  0x31: "Space",
  0x32: "Backquote",
  0x33: "Backspace",
  0x35: "Escape",
  0x7b: "ArrowLeft",
  0x7c: "ArrowRight",
  0x7d: "ArrowDown",
  0x7e: "ArrowUp",
};

/* keys whose characters are AppKit's private-use function characters */
const NAMED: Record<string, string> = {
  Enter: "Enter",
  Tab: "Tab",
  Space: " ",
  Backspace: "Backspace",
  Escape: "Escape",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  ArrowDown: "ArrowDown",
  ArrowUp: "ArrowUp",
};

export function keyEventOf(handed: HandedKey): KeyboardEventInit {
  const code = CODES[handed.keyCode] ?? "";
  return {
    key: NAMED[code] ?? handed.key,
    code,
    metaKey: handed.meta,
    shiftKey: handed.shift,
    altKey: handed.alt,
    ctrlKey: handed.ctrl,
    bubbles: true,
    cancelable: true,
  };
}
