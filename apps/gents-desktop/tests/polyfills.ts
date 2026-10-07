/* jsdom's gaps, filled before the setup and any test module load: modules
   read some of these when they are imported. A setup file of its own, first,
   since imports are hoisted above a file's statements. */
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
  /* jsdom has no PointerEvent; Base UI's floating focus manager builds one */
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
