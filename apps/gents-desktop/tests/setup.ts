import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterAll, afterEach, beforeAll } from "vitest";

const originalConsoleError = console.error.bind(console);

afterEach(async () => {
  cleanup();
  /* imported here rather than above, so a test can still mock what the
     stores import (the platform, the window) */
  (await import("../src/ui/preferences")).resetPreferences();
  (await import("../src/ui/app/listViews")).resetListViews();
});

beforeAll(() => {
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
