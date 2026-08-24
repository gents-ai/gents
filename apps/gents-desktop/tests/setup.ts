import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterAll, afterEach, beforeAll } from "vitest";

const originalConsoleError = console.error.bind(console);

afterEach(() => {
  cleanup();
});

beforeAll(() => {
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
  console.error = (...args: unknown[]) => {
    if (
      typeof args[0] === "string" &&
      (args[0].includes("Expected static flag was missing") ||
        args[0].includes('Each child in a list should have a unique "key" prop.'))
    ) {
      return;
    }
    originalConsoleError(...args);
  };
});

afterAll(() => {
  console.error = originalConsoleError;
});
