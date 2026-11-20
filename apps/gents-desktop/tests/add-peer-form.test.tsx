import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AddPeerForm, type AddPeerFormProps } from "@source-inc/gents-desktop-fleet";
import type { ClientAccessRequestView } from "@source-inc/gents-desktop-client";

const enrollmentRequest: ClientAccessRequestView = {
  requestId: "enrollment-request-1",
  nodeDid: "did:key:z6MkAmy",
  clientDid: "did:key:client",
  state: "pending",
};

function renderForm(overrides: Partial<AddPeerFormProps> = {}) {
  const props: AddPeerFormProps = {
    addingPeer: false,
    disabled: false,
    localError: null,
    onRequestRuntimeAccess: vi.fn(async () => enrollmentRequest),
    ...overrides,
  };
  render(<AddPeerForm {...props} />);
  return props;
}

describe("AddPeerForm", () => {
  it("shows the runtime address as the remote connection entry point", () => {
    renderForm();

    const statusForm = screen.getByTestId("fleet-status-form");
    expect(statusForm).toHaveTextContent("Recommended");
    expect(screen.queryByText("Use a signed pairing invite")).not.toBeInTheDocument();
  });

  it("requests access without creating a runtime peer", async () => {
    const props = renderForm({
      onRequestRuntimeAccess: vi.fn(async () => enrollmentRequest),
    });
    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-fetch-status"));

    await waitFor(() => {
      expect(props.onRequestRuntimeAccess).toHaveBeenCalledWith(
        "http://127.0.0.1:9181",
      );
      expect(screen.getByTestId("fleet-import-status")).toHaveTextContent(
        "Access request enrollment-request-1 sent · waiting for administrator approval",
      );
    });
  });

  it("renders a local error", () => {
    renderForm({ localError: "peer already exists" });
    expect(screen.getByText("peer already exists")).toBeInTheDocument();
  });
});
