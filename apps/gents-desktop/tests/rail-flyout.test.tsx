import { fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RailFlyout } from "@/app/RailFlyout";
import { renderIn, testApp } from "./app-fixture";

describe("the rail flyout", () => {
  afterEach(() => vi.useRealTimers());

  it("leaves no timer behind when it unmounts", () => {
    // jsdom queues storage events while initializing the app.
    const app = testApp();
    vi.useFakeTimers();
    const { unmount } = renderIn(
      app,
      <RailFlyout route={{ name: "sessions" }} settings={null}>
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
