import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RailFlyout } from "@/app/RailFlyout";

describe("the rail flyout", () => {
  afterEach(() => vi.useRealTimers());

  it("leaves no timer behind when it unmounts", () => {
    vi.useFakeTimers();
    const { unmount } = render(
      <RailFlyout
        route={{ name: "sessions" }}
        agentName={null}
        agentDid={null}
        deployment={null}
        online={false}
        mailboxCount={0}
      >
        <button type="button">rail</button>
      </RailFlyout>,
    );
    fireEvent.blur(screen.getByRole("button", { name: "rail" }), {
      relatedTarget: null,
    });
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });
});
