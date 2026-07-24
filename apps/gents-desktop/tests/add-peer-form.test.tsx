import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  AddPeerForm,
  type AddPeerFormProps,
} from "../src/components/fleet/AddPeerForm";
import type { BearerPairingResponse, PeerAddRequest } from "../src/lib/types";

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
    onFetchPeerStatus: vi.fn(async () => ({})),
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
  it("pairs from a signed bearer invite", async () => {
    const props = renderForm();
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
      expect(screen.getByTestId("fleet-pair-status")).toHaveTextContent(
        "Amy is ready",
      );
    });
  });

  it("submits a complete manual peer via onSubmit without fetching /status", async () => {
    const props = renderForm();
    fireEvent.click(screen.getByTestId("fleet-add-submit"));
    await waitFor(() => {
      expect(props.onSubmit).toHaveBeenCalledWith({
        label: "worker-a",
        agentDid: "did:key:z6MkWorkerA",
        addr: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
        graphql: "http://127.0.0.1:9181/api/v0/graphql",
      });
    });
    expect(props.onFetchPeerStatus).not.toHaveBeenCalled();
  });

  it("fetches /status then submits the discovered peer when the manual triple is incomplete", async () => {
    // /status returns a runtime descriptor; the form parses it into a peer and
    // submits THAT (fetch -> parse -> submit), not the empty manual form.
    const discovered = {
      agent_name: "discovered-worker",
      agent_did: "did:key:z6MkDiscovered",
      p2p_shareable_address: "/ip4/1.2.3.4/tcp/9161/p2p/12D3KooDiscovered",
    };
    const props = renderForm({
      peerForm: { label: "", agentDid: "", addr: "", graphql: null },
      onFetchPeerStatus: vi.fn(async () => discovered),
    });
    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-add-submit"));

    await waitFor(() => {
      expect(props.onFetchPeerStatus).toHaveBeenCalledWith("http://127.0.0.1:9181");
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
