import { render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { StreamText } from "@/screens/StreamText";

const shown = "The reply streams in and the reader sees all of this part.";
const more = `${shown} ${"Then it keeps going for a good while longer. ".repeat(8)}`;

beforeEach(() => {
  vi.stubGlobal(
    "matchMedia",
    vi.fn().mockImplementation((query: string) => ({
      matches: false,
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
});
afterEach(() => vi.unstubAllGlobals());

describe("a streamed reply drawn again", () => {
  it("resumes from what was already shown", async () => {
    const first = render(<StreamText text={shown} revealKey="reply-resume" />);
    await waitFor(() => expect(first.container.textContent).toBe(shown), {
      timeout: 4_000,
    });
    first.unmount();

    const { container } = render(<StreamText text={more} revealKey="reply-resume" />);
    /* its first frame holds what the reader had, not an empty row */
    expect(container.textContent?.length ?? 0).toBeGreaterThan(shown.length - 12);
    expect(container.textContent?.length ?? 0).toBeLessThan(more.trim().length);
  });

  it("types from the start when nothing of it was shown", () => {
    const { container } = render(<StreamText text={more} revealKey="reply-new" />);
    expect(container.textContent ?? "").toBe("");
  });
});
