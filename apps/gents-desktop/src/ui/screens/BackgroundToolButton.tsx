import { useState } from "react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { useSelectedSessionFields } from "../hooks/useSelectedSession";
import { toastFailure } from "@/lib/failure";

export function BackgroundToolButton({ tool }: { tool: RenderedToolCallView }) {
  const { actions } = useApp();
  const session = useSelectedSessionFields((value) =>
    value ? { nodeDid: value.nodeDid, sessionId: value.sessionId } : null,
  );
  const nodeDid = session?.nodeDid;
  const sessionId = session?.sessionId;
  const [requested, setRequested] = useState(false);
  if (tool.awaitMode === "background") return <span>In background</span>;
  if (
    tool.statusKind !== "running" ||
    tool.backgroundAvailable !== true ||
    !tool.toolCallId ||
    !tool.requestId ||
    !nodeDid ||
    !sessionId
  )
    return null;

  const background = async () => {
    setRequested(true);
    try {
      await actions.backgroundTool({
        nodeDid,
        sessionId,
        requestId: tool.requestId!,
        toolCallId: tool.toolCallId!,
      });
    } catch (error) {
      toastFailure("run the command in the background", error);
    } finally {
      setRequested(false);
    }
  };
  return (
    <Button
      variant="quiet"
      size="xs"
      disabled={requested}
      onClick={(event) => {
        event.stopPropagation();
        void background();
      }}
    >
      {requested ? "Backgrounding…" : "Run in background"}
    </Button>
  );
}
