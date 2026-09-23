import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { useDraft } from "../src/ui/screens/agent/draft";
import { DraftActions } from "../src/ui/screens/agent/editors";

function Harness({
  persist,
  saved = { name: "Saved", enabled: true },
}: {
  persist: (next: { name: string; enabled: boolean }) => Promise<unknown>;
  saved?: { name: string; enabled: boolean };
}) {
  const draft = useDraft(saved, persist);
  return (
    <>
      <output data-testid="name">{draft.draft.name}</output>
      <output data-testid="enabled">{String(draft.draft.enabled)}</output>
      <button onClick={() => draft.set("name", "Draft")}>Edit text</button>
      <button onClick={() => draft.choose("enabled", false)}>Edit choice</button>
      <button onClick={draft.commit}>Blur field</button>
      <DraftActions
        dirty={draft.dirty}
        saving={draft.saving}
        error={draft.error}
        onSave={draft.save}
        onCancel={draft.reset}
      />
    </>
  );
}

describe("configuration drafts", () => {
  it("does not persist text or choices until Save and Cancel restores the baseline", async () => {
    const user = userEvent.setup();
    const persist = vi.fn(async () => undefined);
    render(<Harness persist={persist} />);

    await user.click(screen.getByRole("button", { name: "Edit text" }));
    await user.click(screen.getByRole("button", { name: "Edit choice" }));
    await user.click(screen.getByRole("button", { name: "Blur field" }));

    expect(persist).not.toHaveBeenCalled();
    expect(screen.getByTestId("name")).toHaveTextContent("Draft");
    expect(screen.getByTestId("enabled")).toHaveTextContent("false");

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByTestId("name")).toHaveTextContent("Saved");
    expect(screen.getByTestId("enabled")).toHaveTextContent("true");
    expect(persist).not.toHaveBeenCalled();
  });

  it("persists the complete reviewed draft and exposes save errors", async () => {
    const user = userEvent.setup();
    const persist = vi
      .fn<(next: { name: string; enabled: boolean }) => Promise<unknown>>()
      .mockRejectedValueOnce(new Error("Name is required"))
      .mockResolvedValueOnce(undefined);
    render(<Harness persist={persist} />);

    await user.click(screen.getByRole("button", { name: "Edit text" }));
    await user.click(screen.getByRole("button", { name: "Edit choice" }));
    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Name is required");
    expect(persist).toHaveBeenLastCalledWith(
      { name: "Draft", enabled: false },
      undefined,
    );

    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(persist).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("accepts fresh snapshots without discarding an unsaved draft", async () => {
    const user = userEvent.setup();
    const persist = vi.fn(async () => undefined);
    const view = render(<Harness persist={persist} />);

    view.rerender(
      <Harness persist={persist} saved={{ name: "Remote", enabled: false }} />,
    );
    expect(screen.getByTestId("name")).toHaveTextContent("Remote");
    expect(screen.getByTestId("enabled")).toHaveTextContent("false");

    await user.click(screen.getByRole("button", { name: "Edit text" }));
    view.rerender(
      <Harness persist={persist} saved={{ name: "New remote", enabled: true }} />,
    );
    expect(screen.getByTestId("name")).toHaveTextContent("Draft");

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByTestId("name")).toHaveTextContent("New remote");
    expect(screen.getByTestId("enabled")).toHaveTextContent("true");
  });
});

describe("draft save results", () => {
  function Probe({
    persist,
    isNew,
    onResult,
  }: {
    persist: (next: { name: string }) => Promise<unknown>;
    isNew?: boolean;
    onResult: (ok: boolean) => void;
  }) {
    const draft = useDraft({ name: "Prefilled" }, persist, { isNew });
    return (
      <>
        <output data-testid="dirty">{String(draft.dirty)}</output>
        <button onClick={() => void draft.save().then(onResult)}>Save it</button>
      </>
    );
  }

  it("resolves false on a failed persist and true once it succeeds", async () => {
    const user = userEvent.setup();
    const onResult = vi.fn();
    const persist = vi
      .fn<(next: { name: string }) => Promise<unknown>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce(undefined);
    render(<Probe persist={persist} isNew onResult={onResult} />);
    await user.click(screen.getByRole("button", { name: "Save it" }));
    expect(onResult).toHaveBeenLastCalledWith(false);
    await user.click(screen.getByRole("button", { name: "Save it" }));
    expect(onResult).toHaveBeenLastCalledWith(true);
    expect(screen.getByTestId("dirty")).toHaveTextContent("false");
  });

  it("treats an untouched new document as saveable and an untouched saved one as not", async () => {
    const user = userEvent.setup();
    const persist = vi.fn(async () => undefined);
    const onResult = vi.fn();
    const view = render(<Probe persist={persist} isNew onResult={onResult} />);
    expect(screen.getByTestId("dirty")).toHaveTextContent("true");
    view.unmount();
    render(<Probe persist={persist} onResult={onResult} />);
    expect(screen.getByTestId("dirty")).toHaveTextContent("false");
    await user.click(screen.getByRole("button", { name: "Save it" }));
    expect(onResult).toHaveBeenLastCalledWith(false);
    expect(persist).not.toHaveBeenCalled();
  });
});
