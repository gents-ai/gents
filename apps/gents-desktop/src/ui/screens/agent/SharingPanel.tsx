import { FieldRow, FieldRows } from "@gents/ui/components/field";
import { Skeleton } from "@gents/ui/components/skeleton";
import type { SharingState } from "../../../hooks/sharing";
import type { NodeView } from "../../../hooks/fleetStore";
import { useSharing } from "@/hooks/useSharing";
import { historyNote } from "../sharing/History";
import { RequestsStrip } from "../sharing/Requests";
import { missingText, SharingTabs } from "../sharing/SharingTabs";

/** Who may use the node: requests waiting for a decision at the top, then
    who has access and what was decided, one at a time. The desktop decides
    for the node it runs; what the node's runtime doesn't offer yet is named
    in one line rather than shown empty. */
export function SharingPanel({ deployment }: { deployment: NodeView }) {
  const name = deployment.node.displayName ?? deployment.label;
  const view = useSharing(deployment.nodeDid);
  return (
    <div data-testid="sharing-panel">
      <header className="mb-6">
        <h2 className="font-heading text-lg text-heading">Sharing</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          Who can use {name}, and how they got in.
        </p>
      </header>
      {view.status === "loading" ? (
        <SharingLoading />
      ) : view.status === "error" ? (
        <p role="alert" className="text-sm break-words text-destructive">
          Couldn't read who may use {name}: {view.message}
        </p>
      ) : view.status === "unavailable" ? (
        <Quiet>
          {`Sharing isn't available for ${name} from this desktop. Requests to join are decided on the node they ask for.`}
        </Quiet>
      ) : (
        <Shared nodeDid={deployment.nodeDid} name={name} state={view.state} />
      )}
    </div>
  );
}

function Shared({
  nodeDid,
  name,
  state,
}: {
  nodeDid: string;
  name: string;
  state: SharingState;
}) {
  const { available, requests, members, history, historyLimits } = state;
  const missing = missingText(available, name);
  const note = missing && <Quiet>{missing}</Quiet>;
  const historyLeftOut = historyNote(historyLimits);
  const nothing =
    requests.length + members.length + history.length === 0 && historyLeftOut === null;
  return (
    <>
      <RequestsStrip nodeDid={nodeDid} nodeName={name} requests={requests} />
      {nothing ? (
        <>
          <Nothing
            line={
              available.members
                ? "Nothing is shared yet."
                : `Nobody is waiting to join ${name}.`
            }
          />
          {note && <div className="mt-3">{note}</div>}
        </>
      ) : (
        <SharingTabs
          nodeDid={nodeDid}
          name={name}
          available={available}
          members={members}
          history={history}
          historyLeftOut={historyLeftOut}
          note={note}
        />
      )}
    </>
  );
}

/* the one empty state */
function Nothing({ line }: { line: string }) {
  return (
    <div
      data-testid="sharing-empty"
      className="grid justify-items-center gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-10 text-center"
    >
      <p className="text-sm text-muted-foreground">{line}</p>
    </div>
  );
}

function Quiet({ children }: { children: string }) {
  return <p className="text-xs text-muted-foreground">{children}</p>;
}

function SharingLoading() {
  return (
    <FieldRows aria-label="Loading sharing" aria-busy="true" className="mb-8">
      {[0, 1].map((i) => (
        <FieldRow key={i} className="flex-col items-stretch gap-1.5">
          <Skeleton className="h-3.5 w-48 max-w-full" />
          <Skeleton className="h-3 w-72 max-w-full" />
        </FieldRow>
      ))}
    </FieldRows>
  );
}
