import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Reply } from "@/screens/Transcript";
import { assistantMessage, liveAssistant } from "./timeline-fixture";

const start = "The reply streams";
const answer = `${start} ${"and keeps going for a good while ".repeat(12)}to its end.`;
const live = liveAssistant({ itemKey: "live", content: start, reasoning: null });
const saved = assistantMessage({
  itemKey: "message-7",
  content: answer,
  timestamp: "2026-11-27T12:00:00Z",
});
const copy = () => screen.queryByRole("button", { name: "Copy" });

function motion(reduced: boolean) {
  vi.stubGlobal(
    "matchMedia",
    vi.fn().mockImplementation((query: string) => ({
      matches: reduced,
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
}

describe("a reply's copy button and time", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("wait for a streamed reply's text to finish revealing", async () => {
    motion(false);
    const { rerender } = render(<Reply item={live} />);
    await waitFor(() => expect(screen.getByText(start)).toBeInTheDocument());

    rerender(<Reply item={saved} />);
    expect(screen.queryByText(answer)).not.toBeInTheDocument();
    expect(copy()).not.toBeInTheDocument();

    await waitFor(() => expect(screen.getByText(answer)).toBeInTheDocument(), {
      timeout: 4_000,
    });
    await waitFor(() => expect(copy()).toBeInTheDocument());
  });

  it("show at once for a reply that never streamed", () => {
    motion(false);
    render(<Reply item={saved} />);
    expect(screen.getByText(answer)).toBeInTheDocument();
    expect(copy()).toBeInTheDocument();
  });

  it("show at once when a streamed reply had no text until it was saved", () => {
    motion(false);
    const { rerender } = render(
      <Reply item={liveAssistant({ itemKey: "live", content: "", reasoning: null })} />,
    );
    rerender(<Reply item={saved} />);
    expect(copy()).toBeInTheDocument();
  });

  it("show at once with reduced motion", () => {
    motion(true);
    const { rerender } = render(<Reply item={live} />);
    rerender(<Reply item={saved} />);
    expect(screen.getByText(answer)).toBeInTheDocument();
    expect(copy()).toBeInTheDocument();
  });
});
