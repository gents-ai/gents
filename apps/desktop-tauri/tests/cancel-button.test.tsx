import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { CancelButton } from "../src/components/cancelUx";

describe("CancelButton", () => {
  it("renders nothing when no turn is in flight", () => {
    const { container } = render(
      <CancelButton
        activeRequestId="req_a17"
        turnState="idle"
        onInterruptClick={() => {}}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders enabled when a turn is streaming and activeRequestId is known", () => {
    render(
      <CancelButton
        activeRequestId="req_a17"
        turnState="streaming"
        onInterruptClick={() => {}}
      />,
    );
    const btn = screen.getByRole("button", { name: /interrupt/i });
    expect(btn).toBeEnabled();
  });

  it("renders disabled with waiting-for-turn copy when turn active but activeRequestId is null", () => {
    render(
      <CancelButton
        activeRequestId={null}
        turnState="streaming"
        onInterruptClick={() => {}}
      />,
    );
    const btn = screen.getByRole("button", { name: /interrupt/i });
    expect(btn).toBeDisabled();
    expect(btn).toHaveAttribute(
      "title",
      expect.stringMatching(/waiting for turn to register/i),
    );
  });

  it("calls onInterruptClick when clicked", () => {
    const handler = vi.fn();
    render(
      <CancelButton
        activeRequestId="req_a17"
        turnState="streaming"
        onInterruptClick={handler}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /interrupt/i }));
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("does not call onInterruptClick when disabled", () => {
    const handler = vi.fn();
    render(
      <CancelButton
        activeRequestId={null}
        turnState="streaming"
        onInterruptClick={handler}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /interrupt/i }));
    expect(handler).not.toHaveBeenCalled();
  });

  it("forceVisible renders the button even when turnState is idle", () => {
    render(
      <CancelButton
        activeRequestId="req_a17"
        turnState="idle"
        onInterruptClick={() => {}}
        forceVisible
      />,
    );
    expect(screen.getByRole("button", { name: /interrupt/i })).toBeInTheDocument();
  });

  it("renders enabled when turnState is waitingForClaim", () => {
    render(
      <CancelButton
        activeRequestId="req_a17"
        turnState="waitingForClaim"
        onInterruptClick={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: /interrupt/i })).toBeEnabled();
  });
});
