import { useRef, useState } from "react";
import { Terminal } from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { useSelectedSessionFields } from "../hooks/useSelectedSession";
import { toastFailure } from "@/lib/failure";
import { openBackgroundLog } from "./background";

export function BackgroundToolButton({ tool }: { tool: RenderedToolCallView }) {
  const { actions } = useApp();
  const session = useSelectedSessionFields((value) =>
    value ? { nodeDid: value.nodeDid, sessionId: value.sessionId } : null,
  );
  const nodeDid = session?.nodeDid;
  const sessionId = session?.sessionId;
  const [requested, setRequested] = useState(false);
  const inFlight = useRef(false);
  if (tool.awaitMode === "background")
    return (
      <>
        <span>In background</span>
        {sessionId && tool.requestId && tool.toolCallId && (
          <Button
            variant="quiet"
            size="xs"
            className="ml-1.5"
            onClick={(event) => {
              event.stopPropagation();
              openBackgroundLog(sessionId, tool.requestId!, tool.toolCallId!);
            }}
          >
            <Terminal />
            View output
          </Button>
        )}
      </>
    );
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
    if (inFlight.current || requested) return;
    inFlight.current = true;
    setRequested(true);
    try {
      setRequested(
        await actions.backgroundTool({
          nodeDid,
          sessionId,
          requestId: tool.requestId!,
          toolCallId: tool.toolCallId!,
        }),
      );
    } catch (error) {
      setRequested(false);
      toastFailure("run the command in the background", error);
    } finally {
      inFlight.current = false;
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
