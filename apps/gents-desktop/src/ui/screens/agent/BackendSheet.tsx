/* A backend's full editor beside the page that references it. */
import type { NodeView } from "../../../hooks/fleetStore";
import { EditorSheet } from "./EditorSheet";
import { BackendEditor } from "./BackendEditor";
import { useAccounts } from "@/hooks/useProviders";

export function BackendSheet({
  deployment,
  backendId,
  onClose,
}: {
  deployment: NodeView;
  backendId: string | null;
  onClose: () => void;
}) {
  const { accounts } = useAccounts(deployment.agentDid);
  const backend =
    deployment.inferenceBackends.find((b) => b.backendId === backendId) ?? null;
  return (
    <EditorSheet
      open={backend !== null}
      onClose={onClose}
      title={backend?.name ?? backend?.backendId ?? "Backend"}
      description={
        backend
          ? [
              backend.endpoint,
              (() => {
                const n = deployment.inferenceProfiles.filter(
                  (p) => p.backend_id === backend.backendId,
                ).length;
                return `serves ${n} ${n === 1 ? "profile" : "profiles"}`;
              })(),
            ]
              .filter(Boolean)
              .join(" · ")
          : undefined
      }
      page={
        backend
          ? {
              name: "agent",
              agentDid: deployment.agentDid,
              section: "inference",
              item: backend.backendId,
            }
          : undefined
      }
    >
      {backend && (
        <BackendEditor
          key={backend.backendId}

          deployment={deployment}
          backend={backend}
          accounts={accounts}
          embedded
        />
      )}
    </EditorSheet>
  );
}
