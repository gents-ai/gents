import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AddPeerForm, type AddPeerFormProps } from "@source-inc/gents-desktop-fleet";
import type {
  BearerPairingResponse,
  PeerAddRequest,
} from "@source-inc/gents-desktop-client";

function renderForm(overrides: Partial<AddPeerFormProps> = {}) {
  const peerForm: PeerAddRequest = {
    label: "worker-a",
    agentDid: "did:key:z6MkWorkerA",
    addr: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
    graphql: "http://127.0.0.1:9181/api/v0/graphql",
  };
  const props: AddPeerFormProps = {
    addingPeer: false,
    disabled: false,
    localError: null,
    peerForm,
    onPeerFormChange: vi.fn(),
    onProbePeerAddress: vi.fn(async () => ({})),
    onPairBearer: vi.fn(async () => ({
      bootstrap: {} as BearerPairingResponse["bootstrap"],
      client: null,
      pairing: {
        peerId: "peer-amy",
        label: "Amy",
        addr: "iroh://amy",
        issuerDid: "did:key:zAmy",
        claimantDid: "did:key:zPhone",
        networkId: "amy-network",
        template: "conversation",
        connected: true,
        claimSubmitted: true,
        endpointPublished: true,
        replicationConfigured: true,
        membershipObserved: true,
        bidirectionalReplicationObserved: true,
      },
    })),
    onSubmit: vi.fn(async () => undefined),
    ...overrides,
  };
  render(<AddPeerForm {...props} />);
  return props;
}

describe("AddPeerForm", () => {
  it("shows server status connection before collapsed alternatives", () => {
    renderForm();

    const statusForm = screen.getByTestId("fleet-status-form");
    const manual = screen
      .getByText("Enter connection details manually")
      .closest("details");
    const signed = screen.getByText("Use a signed pairing invite").closest("details");

    expect(statusForm).toHaveTextContent("Recommended");
    expect(manual).not.toHaveAttribute("open");
    expect(signed).not.toHaveAttribute("open");
    expect(
      statusForm.compareDocumentPosition(manual!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      statusForm.compareDocumentPosition(signed!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("pairs from a signed bearer invite", async () => {
    const props = renderForm();
    fireEvent.click(screen.getByText("Use a signed pairing invite"));
    fireEvent.change(screen.getByTestId("fleet-pair-label"), {
      target: { value: "Amy" },
    });
    fireEvent.change(screen.getByTestId("fleet-pair-token"), {
      target: { value: "dabear1-signed-invite" },
    });
    fireEvent.click(screen.getByTestId("fleet-pair-submit"));

    await waitFor(() => {
      expect(props.onPairBearer).toHaveBeenCalledWith({
        token: "dabear1-signed-invite",
        label: "Amy",
      });
      expect(screen.getByTestId("fleet-pair-status")).toHaveTextContent("Amy is ready");
    });
  });

  it("submits a complete manual peer via onSubmit without fetching /status", async () => {
    const props = renderForm();
    fireEvent.click(screen.getByText("Enter connection details manually"));
    fireEvent.click(screen.getByTestId("fleet-add-submit"));
    await waitFor(() => {
      expect(props.onSubmit).toHaveBeenCalledWith({
        label: "worker-a",
        agentDid: "did:key:z6MkWorkerA",
        addr: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
        graphql: "http://127.0.0.1:9181/api/v0/graphql",
      });
    });
    expect(props.onProbePeerAddress).not.toHaveBeenCalled();
  });

  it("contains a rejected manual peer submission after the shell records it", async () => {
    const onSubmit = vi.fn(async () => {
      throw new Error("peer rejected");
    });
    renderForm({ onSubmit });
    fireEvent.click(screen.getByText("Enter connection details manually"));
    fireEvent.click(screen.getByTestId("fleet-add-submit"));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
  });

  it("fetches /status and submits the discovered peer in one action", async () => {
    const discovered = {
      agent_name: "discovered-worker",
      agent_did: "did:key:z6MkDiscovered",
      p2p_shareable_address: "/ip4/1.2.3.4/tcp/9161/p2p/12D3KooDiscovered",
    };
    const props = renderForm({
      peerForm: { label: "", agentDid: "", addr: "", graphql: null },
      onProbePeerAddress: vi.fn(async () => discovered),
    });
    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-fetch-status"));

    await waitFor(() => {
      expect(props.onProbePeerAddress).toHaveBeenCalledWith("http://127.0.0.1:9181");
    });
    await waitFor(() => {
      expect(props.onSubmit).toHaveBeenCalledWith({
        label: "discovered-worker",
        agentDid: "did:key:z6MkDiscovered",
        addr: "/ip4/1.2.3.4/tcp/9161/p2p/12D3KooDiscovered",
      });
    });
  });

  it("renders a local error", () => {
    renderForm({ localError: "peer already exists" });
    expect(screen.getByText("peer already exists")).toBeInTheDocument();
  });
});
