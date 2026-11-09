import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach } from "vitest";
import {
  memorySendAttachments,
  setSendAttachments,
} from "../src/hooks/sendAttachments";

const originalConsoleError = console.error.bind(console);

/* jsdom has no IndexedDB; each test keeps its sends' attachments in memory */
beforeEach(() => setSendAttachments(memorySendAttachments()));

afterEach(async () => {
  cleanup();
  (await import("../src/ui/preferences")).resetPreferences();
  (await import("../src/ui/app/listViews")).resetListViews();
  (await import("../src/ui/app/mailboxSeen")).resetMailboxSeen();
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
