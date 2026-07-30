import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "../src/App";
import { ErrorBoundary } from "../src/components/ErrorBoundary";

function Thrower(): never {
  throw new Error("boom from render");
}

describe("ErrorBoundary", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders children when nothing throws", () => {
    render(
      <ErrorBoundary>
        <span data-testid="happy">ok</span>
      </ErrorBoundary>,
    );
    expect(screen.getByTestId("happy")).toBeInTheDocument();
  });

  it("catches render errors and offers reload instead of white-screening", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const reload = vi.fn();
    const original = window.location;
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...original, reload },
    });

    try {
      render(
        <ErrorBoundary>
          <Thrower />
        </ErrorBoundary>,
      );

      expect(screen.getByTestId("error-boundary")).toBeInTheDocument();
      const details = screen.getByTestId("error-boundary").querySelector("pre");
      expect(details?.textContent).toContain("boom from render");
      expect(details?.textContent).toContain("Component stack:");
      expect(details?.textContent).toContain("Thrower");

      fireEvent.click(screen.getByTestId("error-boundary-reload"));
      expect(reload).toHaveBeenCalled();
    } finally {
      Object.defineProperty(window, "location", {
        configurable: true,
        value: original,
      });
    }
  });

  it("App wraps the shell in the boundary (the actual white-screen fix)", () => {
    expect(App({}).type).toBe(ErrorBoundary);
  });
});
