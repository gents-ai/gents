/* jsdom gaps, filled before any test module load (imports are hoisted above
   a setup file's statements). */
if (typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string) =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList;
}
if (typeof globalThis.ResizeObserver !== "function") {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}
if (typeof window.PointerEvent !== "function") {
  window.PointerEvent = class extends MouseEvent {
    readonly pointerId: number;
    readonly pointerType: string;
    constructor(type: string, init: PointerEventInit = {}) {
      super(type, init);
      this.pointerId = init.pointerId ?? 1;
      this.pointerType = init.pointerType ?? "mouse";
    }
  } as unknown as typeof PointerEvent;
}
if (typeof Element.prototype.getAnimations !== "function") {
  Element.prototype.getAnimations = () => [];
}
if (!HTMLElement.prototype.scrollIntoView) {
  HTMLElement.prototype.scrollIntoView = () => {};
}
if (!HTMLElement.prototype.scrollTo) {
  HTMLElement.prototype.scrollTo = function (
    options?: ScrollToOptions | number,
    y?: number,
  ) {
    const clamp = (value: number, maximum: number) =>
      Math.min(Math.max(0, value), Math.max(0, maximum));
    if (typeof options === "number") {
      this.scrollLeft = clamp(options, this.scrollWidth - this.clientWidth);
      this.scrollTop = clamp(y ?? 0, this.scrollHeight - this.clientHeight);
    } else {
      this.scrollLeft = clamp(
        options?.left ?? this.scrollLeft,
        this.scrollWidth - this.clientWidth,
      );
      this.scrollTop = clamp(
        options?.top ?? this.scrollTop,
        this.scrollHeight - this.clientHeight,
      );
    }
  };
}
/* the composer's rich editor measures the caret's range to keep it in view */
if (typeof Range.prototype.getBoundingClientRect !== "function") {
  Range.prototype.getBoundingClientRect = () => new DOMRect(0, 0, 0, 0);
}
if (typeof Range.prototype.getClientRects !== "function") {
  Range.prototype.getClientRects = () =>
    ({
      length: 0,
      item: () => null,
      [Symbol.iterator]: [][Symbol.iterator],
    }) as unknown as DOMRectList;
}
/* the composer's rich editor names ClipboardEvent when it handles a paste */
if (typeof globalThis.ClipboardEvent !== "function") {
  globalThis.ClipboardEvent = class extends Event {
    readonly clipboardData: DataTransfer | null;
    constructor(type: string, init: ClipboardEventInit = {}) {
      super(type, init);
      this.clipboardData = init.clipboardData ?? null;
    }
  } as unknown as typeof ClipboardEvent;
}
/* Node defines its own localStorage and sessionStorage globals that shadow
   jsdom's; without a valid --localstorage-file they are undefined (Node 25+)
   or an object without the Storage methods */
for (const name of ["localStorage", "sessionStorage"] as const) {
  if (typeof globalThis[name]?.getItem !== "function") {
    const storage = (globalThis as { jsdom?: { window: Window } }).jsdom?.window[name];
    if (storage)
      Object.defineProperty(globalThis, name, {
        value: storage,
        configurable: true,
        writable: true,
      });
  }
}
