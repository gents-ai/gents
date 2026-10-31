import { Fragment, useMemo } from "react";
import { Chip, parseParts } from "@gents/ui/conversation";
import { navigate } from "@/lib/router";
import { useSelectedNodeDid } from "@/hooks/useClient";
import { useSessionChipKind } from "./useMentionMenu";

/** A sent message's text with its session references drawn as chips, each
    opening its session; other text, links included, stays as written. */
export function SentText({ text }: { text: string }) {
  const nodeDid = useSelectedNodeDid();
  const session = useSessionChipKind(nodeDid);
  const parts = useMemo(() => parseParts(text, { session }), [text, session]);
  if (!parts.some((part) => part.type === "chip")) return text;
  return parts.map((part, index) =>
    part.type === "text" ? (
      <Fragment key={index}>{part.text}</Fragment>
    ) : (
      <Chip
        key={index}
        chip={part.chip}
        kind={session}
        title={`Open ${part.chip.label}`}
        onClick={() =>
          navigate({
            name: "session",
            sessionId: part.chip.id,
            nodeDid: nodeDid ?? undefined,
          })
        }
      />
    ),
  );
}
