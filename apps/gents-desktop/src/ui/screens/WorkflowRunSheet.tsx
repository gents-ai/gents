import { useRef } from "react";
import { EditorSheet } from "./agent/EditorSheet";
import { runTitle, useWorkflowRun, WorkflowRunView } from "./WorkflowRun";

export function WorkflowRunSheet({
  open,
  nodeDid,
  rowDocId,
  observed,
  onClose,
}: {
  open: boolean;
  nodeDid: string;
  rowDocId: string;

  observed: string;
  onClose: () => void;
}) {
  const run = useWorkflowRun({ nodeDid, rowDocId, observed, active: open });
  const answering = useRef(false);
  const close = () => {
    run.reset();
    onClose();
  };
  return (
    <EditorSheet
      open={open}
      onClose={close}
      title={runTitle(run.view)}
      finalFocus={() => {
        if (!answering.current) return true;
        answering.current = false;
        return (
          document.querySelector<HTMLTextAreaElement>(
            '[data-testid="composer"] textarea',
          ) ?? true
        );
      }}
    >
      <WorkflowRunView
        run={run}
        onLeave={(toComposer) => {
          answering.current = toComposer;
          close();
        }}
      />
    </EditorSheet>
  );
}
