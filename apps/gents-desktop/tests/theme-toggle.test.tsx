import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { ThemeToggle } from "../src/components/ThemeToggle";
import { loadTheme } from "../src/lib/theme";

describe("ThemeToggle", () => {
  beforeEach(() => {
    const store = new Map<string, string>();
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      value: {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, value: string) => void store.set(key, String(value)),
        removeItem: (key: string) => void store.delete(key),
      },
    });
    delete document.documentElement.dataset.theme;
  });

  it("defaults to dark and round-trips through light with persistence", () => {
    render(<ThemeToggle />);
    expect(loadTheme()).toBe("dark");

    const toggle = screen.getByTestId("theme-toggle");
    expect(toggle).toHaveAccessibleName("Switch to light theme");

    fireEvent.click(toggle);
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(loadTheme()).toBe("light");
    expect(toggle).toHaveAccessibleName("Switch to dark theme");

    fireEvent.click(toggle);
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(loadTheme()).toBe("dark");
  });

  it("honors a persisted light choice on mount", () => {
    window.localStorage.setItem("gents-desktop-theme", "light");
    render(<ThemeToggle />);
    expect(screen.getByTestId("theme-toggle")).toHaveAccessibleName(
      "Switch to dark theme",
    );
  });
});
