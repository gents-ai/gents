import { act, fireEvent, screen } from "@testing-library/react";
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

  it("stays open under the pointer when a page it opens takes focus", () => {
    const app = testApp();
    vi.useFakeTimers();
    const { container } = renderIn(
      app,
      <>
        <RailFlyout route={{ name: "sessions" }} settings={null} mode="hover">
          <button type="button">rail</button>
        </RailFlyout>
        <textarea aria-label="composer" />
      </>,
    );
    const flyout = container.querySelector<HTMLElement>(".relative.h-full")!;
    const panel = () => container.querySelector("[data-ground]")!;
    fireEvent.pointerEnter(flyout, { pointerType: "mouse" });
    act(() => vi.runAllTimers());
    expect(panel()).toHaveAttribute("aria-hidden", "false");

    /* a session's composer takes focus as its page opens */
    fireEvent.blur(screen.getByRole("button", { name: "rail" }), {
      relatedTarget: screen.getByRole("textbox", { name: "composer" }),
    });
    act(() => vi.runAllTimers());
    expect(panel()).toHaveAttribute("aria-hidden", "false");

    fireEvent.pointerLeave(flyout);
    act(() => vi.runAllTimers());
    expect(panel()).toHaveAttribute("aria-hidden", "true");
  });
});
