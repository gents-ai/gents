import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ErrorBanner } from "../src/components/ErrorBanner";

describe("ErrorBanner", () => {
  it("announces the failure and supports dismiss and copy", () => {
    const onDismiss = vi.fn();
    render(<ErrorBanner message="bridge exploded" onDismiss={onDismiss} />);

    const banner = screen.getByTestId("error-banner");
    expect(banner).toHaveAttribute("role", "alert");
    expect(banner).toHaveTextContent("bridge exploded");
    expect(screen.getByRole("button", { name: "Copy error" })).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("error-banner-dismiss"));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
