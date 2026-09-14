import { useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ManagedRuntimeAuthorityPicker } from "../src/ui/components/ManagedRuntimeAuthority";
import type { ManagedServerAuthorityInput } from "@source-inc/gents-desktop-client";

function Harness({
  validateRoot,
}: {
  validateRoot: (path: string) => Promise<string>;
}) {
  const [ceiling, setCeiling] =
    useState<ManagedServerAuthorityInput["toolCeiling"]>("readwrite");
  const [directory, setDirectory] = useState<string | null>("/Users/A Person");
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <ManagedRuntimeAuthorityPicker
        home="/Users/A Person"
        toolCeiling={ceiling}
        toolRoot={directory}
        onCeilingChange={setCeiling}
        onRootChange={setDirectory}
        validateRoot={validateRoot}
        error={error}
        onError={setError}
      />
      <output data-testid="selected-directory">{directory ?? "unvalidated"}</output>
    </>
  );
}

describe("ManagedRuntimeAuthorityPicker", () => {
  it("prefills the root and offers independent ceiling choices", async () => {
    const user = userEvent.setup();
    const validate = vi.fn();
    render(<Harness validateRoot={validate} />);
    const input = screen.getByLabelText("Tool root");
    const ceiling = screen.getByRole("combobox", { name: "Tool ceiling" });
    expect(input).toHaveValue("/Users/A Person");
    expect(ceiling).toHaveValue("readwrite");
    await user.selectOptions(ceiling, "readonly");
    expect(input).toHaveValue("/Users/A Person");
    await user.selectOptions(ceiling, "meta-only");
    expect(input).toBeDisabled();
    await user.selectOptions(ceiling, "readwrite");
    expect(input).toBeEnabled();
    expect(validate).not.toHaveBeenCalled();
  });

  it("keeps a typed path unselected until native validation canonicalizes it", async () => {
    const user = userEvent.setup();
    const validate = vi.fn(async () => "/private/tmp/a folder");
    render(<Harness validateRoot={validate} />);

    const input = screen.getByLabelText("Tool root");
    await user.clear(input);
    await user.type(input, "/tmp/a folder");
    await user.tab();

    await waitFor(() => expect(validate).toHaveBeenCalledWith("/tmp/a folder"));
    expect(input).toHaveValue("/private/tmp/a folder");
  });

  it("shows validation failures inline without replacing the selected path", async () => {
    const user = userEvent.setup();
    render(
      <Harness
        validateRoot={vi.fn(async () => Promise.reject(new Error("No access")))}
      />,
    );

    await user.clear(screen.getByLabelText("Tool root"));
    await user.type(screen.getByLabelText("Tool root"), "/missing");
    await user.tab();

    expect(await screen.findByText("No access")).toBeInTheDocument();
    expect(screen.getByLabelText("Tool root")).toHaveValue("/missing");
  });

  it("keeps a pending root validation when the ceiling changes", async () => {
    const user = userEvent.setup();
    let finish!: (path: string) => void;
    render(
      <Harness
        validateRoot={() =>
          new Promise<string>((resolve) => {
            finish = resolve;
          })
        }
      />,
    );
    const input = screen.getByLabelText("Tool root");
    await user.clear(input);
    await user.type(input, "/project");
    await user.tab();
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Tool ceiling" }),
      "readonly",
    );
    finish("/canonical/project");
    await waitFor(() => expect(input).toHaveValue("/canonical/project"));
    expect(screen.getByRole("combobox", { name: "Tool ceiling" })).toHaveValue(
      "readonly",
    );
    expect(screen.getByTestId("selected-directory")).toHaveTextContent(
      "/canonical/project",
    );
  });

  it("ignores an older validation that finishes after the current path", async () => {
    const user = userEvent.setup();
    const resolutions = new Map<string, (canonical: string) => void>();
    const validate = vi.fn(
      (path: string) =>
        new Promise<string>((resolve) => {
          resolutions.set(path, resolve);
        }),
    );
    render(<Harness validateRoot={validate} />);

    const input = screen.getByLabelText("Tool root");
    await user.clear(input);
    await user.type(input, "/slow");
    await user.tab();
    expect(screen.getByTestId("selected-directory")).toHaveTextContent("unvalidated");

    await user.click(input);
    await user.clear(input);
    await user.clear(input);
    await user.type(input, "/current");
    await user.tab();
    resolutions.get("/current")?.("/canonical/current");
    await waitFor(() => expect(input).toHaveValue("/canonical/current"));

    resolutions.get("/slow")?.("/canonical/stale");
    await waitFor(() =>
      expect(screen.getByTestId("selected-directory")).toHaveTextContent(
        "/canonical/current",
      ),
    );
    expect(input).toHaveValue("/canonical/current");
  });
});
