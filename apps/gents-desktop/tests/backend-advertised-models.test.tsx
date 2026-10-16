import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BackendEditor } from "../src/ui/screens/agent/BackendEditor";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const editorWith = (models: string[]) => {
  const backend = { ...deployment.inferenceBackends[0]!, models };
  const app = testApp({
    deployments: [{ ...deployment, inferenceBackends: [backend] }],
  });
  const view = renderIn(
    app,
    <BackendEditor deployment={deployment} backend={backend} accounts={[]} />,
  );
  return (next: string[]) =>
    view.rerender(
      <BackendEditor
        deployment={deployment}
        backend={{ ...backend, models: next }}
        accounts={[]}
      />,
    );
};

describe("a backend's advertised models", () => {
  it("lists a long catalog in a bounded box, with its count and a filter", async () => {
    editorWith(Array.from({ length: 20 }, (_, i) => `model-${i + 1}`));
    expect(screen.getByText(/^20 models\./)).toBeInTheDocument();
    const list = screen.getByRole("list", { name: "Advertised models" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(20);
    expect(list.className).toContain("overflow-y-auto");
    await userEvent.type(screen.getByLabelText("Filter models"), "model-2");
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual(["model-2", "model-20"]);
    await userEvent.type(screen.getByLabelText("Filter models"), "x");
    expect(within(list).getByText("No model matches.")).toBeInTheDocument();
  });

  it("drops a filter whose input a shorter catalog hides", async () => {
    const refresh = editorWith(Array.from({ length: 20 }, (_, i) => `model-${i + 1}`));
    await userEvent.type(screen.getByLabelText("Filter models"), "model-2");
    refresh(["alpha", "beta"]);
    expect(screen.queryByLabelText("Filter models")).toBeNull();
    expect(
      within(screen.getByRole("list", { name: "Advertised models" })).getAllByRole(
        "listitem",
      ),
    ).toHaveLength(2);
  });

  it("shows a short catalog without a filter", () => {
    editorWith(["model-a", "model-b"]);
    expect(screen.getByText(/^2 models\./)).toBeInTheDocument();
    expect(screen.queryByLabelText("Filter models")).toBeNull();
  });

  it("says how to read a catalog the backend has not advertised yet", () => {
    editorWith([]);
    expect(
      screen.getByText("No catalog yet. Refresh models to read it."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Advertised models" })).toBeNull();
  });
});
