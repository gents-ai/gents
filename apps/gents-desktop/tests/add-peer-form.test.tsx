import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AddPeerForm, type AddPeerFormProps } from "@source-inc/gents-desktop-fleet";
import type { EnrollmentRequestView } from "@source-inc/gents-desktop-client";

const enrollmentRequest: EnrollmentRequestView = {
  requestId: "enrollment-request-1",
  networkId: "network-amy",
  adminDid: "did:key:z6MkAmy",
  serverPeer: "server-peer-amy",
  ownerAgent: "did:key:z6MkAmy",
  state: "pending",
};

function renderForm(overrides: Partial<AddPeerFormProps> = {}) {
  const props: AddPeerFormProps = {
    addingPeer: false,
    disabled: false,
    localError: null,
    onRequestStatusEnrollment: vi.fn(async () => enrollmentRequest),
    ...overrides,
  };
  render(<AddPeerForm {...props} />);
  return props;
}

describe("AddPeerForm", () => {
  it("shows authenticated status enrollment as the only remote authority", () => {
    renderForm();

    const statusForm = screen.getByTestId("fleet-status-form");
    expect(statusForm).toHaveTextContent("Recommended");
    expect(screen.queryByText("Use a signed pairing invite")).not.toBeInTheDocument();
  });

  it("requests authenticated enrollment without creating a raw peer", async () => {
    const props = renderForm({
      onRequestStatusEnrollment: vi.fn(async () => enrollmentRequest),
    });
    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-fetch-status"));

    await waitFor(() => {
      expect(props.onRequestStatusEnrollment).toHaveBeenCalledWith(
        "http://127.0.0.1:9181",
      );
      expect(screen.getByTestId("fleet-import-status")).toHaveTextContent(
        "Enrollment request enrollment-request-1 sent",
      );
    });
  });

  it("renders a local error", () => {
    renderForm({ localError: "peer already exists" });
    expect(screen.getByText("peer already exists")).toBeInTheDocument();
  });
});
