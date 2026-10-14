/* The foot of an open session: what the person reads and does next, over
   the composer (the goal, why sending waits) and the line under it. It
   holds the draft, so a keystroke renders this and not the transcript
   above it. */
import type { RefCallback } from "react";
import { ArrowDown } from "lucide-react";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Composer } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { useApp, useView } from "@/app/AppContext";
import { useChatFolder, useInterruptVisible, useSelectedNode } from "@/hooks/useClient";
import { agentOf } from "@/lib/agents";
import { placeholderFor } from "@/lib/send-status";
import { ChatFolderPicker } from "../ChatFolderPicker";
import { LoadingStatus } from "../LoadingStatus";
import { ReplyingTo } from "../ReplyingTo";
import { Goal } from "../SessionHeader";
import { SlashSkillMenu } from "../SlashSkillMenu";
import { useSlashSkills } from "../useSlashSkills";
import { SessionSubmissionStatus, presentedComposerSendStatus } from "./composerParts";
import { useComposerSend } from "./useComposerSend";

export function OpenSessionComposer({
  roomRef,
  atBottom,
  toBottom,
  stop,
  stopping,
  goal,
  agentId,
}: {
  /** where the foot publishes the room it takes (`useComposerRoom`) */
  roomRef: RefCallback<HTMLDivElement>;
  atBottom: boolean;
  toBottom: () => void;
  stop: () => void;
  stopping: boolean;
  goal: DesktopSessionSnapshot["goal"];
  agentId: string | null;
}) {
  const {
    stores,
    actions: { setChatFolder },
  } = useApp();
  const activityStatus = useView((view) => view.shellProjection.activityStatus);
  const sendStatus = useView((view) => view.shellProjection.nonEmptyContentSendStatus);
  const loadingStatus = useView((view) => view.loadingStatus);
  const sending = stores.chat.use.sending();
  const inFlight = useInterruptVisible();
  const chatFolder = useChatFolder();
  const deployment = useSelectedNode();
  const { draft, setDraft, send } = useComposerSend(agentId);
  const agent = agentOf(deployment, agentId);
  const slash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    deployment?.contexts.find((c) => c.context_id === agent?.contextId),
  );

  /* Local text plus the canonical shell admission decision. */
  const status = presentedComposerSendStatus(draft, sendStatus);
  const queueing = status.kind === "queue";
  const blocked = sendStatus.kind === "disabled" && !inFlight;
  /* a status that explains why the session cannot take a message covers
     the composer it disables */
  const covered = blocked && loadingStatus !== null;

  /* Its height is published as --composer-h, so a block that sticks to the
     foot of the scroller can sit clear of it rather than hard-coding what
     the composer happens to be. The composer owns the foot of the
     scroller: anything else that sticks there passes behind it rather than
     over it, however short the block it belongs to turns out to be. */
  return (
    <div ref={roomRef} className="sticky bottom-0 z-20 mt-auto bg-background pt-6 pb-6">
      {/* a goal outlives the turns under it, so it sits with the
          next one rather than at the top where it scrolls away */}
      {goal && <Goal goal={goal} />}
      {/* conversation still running on under the composer: a short
          fade on its top edge says the transcript has not ended,
          where a hard edge reads as the end of it */}
      <div
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-x-0 bottom-full h-8 bg-gradient-to-t from-background to-transparent transition-opacity duration-200",
          atBottom ? "opacity-0" : "opacity-100",
        )}
      />
      {/* the way back sits on the footer's top edge, whatever the footer holds */}
      {!atBottom && (
        <Button
          variant="raised"
          size="icon-sm"
          aria-label="Back to bottom"
          onClick={toBottom}
          className="absolute top-0 left-1/2 z-10 -translate-x-1/2 -translate-y-1/2 rounded-full shadow-md"
        >
          <ArrowDown />
        </Button>
      )}
      {!covered && <LoadingStatus />}
      {/* the card that covers the composer shares its cell, so a
          card taller than the composer grows the cell, not clips */}
      <div data-testid="composer" className="grid">
        <div
          inert={covered || undefined}
          className={cn("[grid-area:1/1]", covered && "invisible")}
        >
          <Composer
            value={draft}
            onChange={setDraft}
            onSend={send}
            models={[]}
            disabled={blocked}
            above={
              <>
                <ReplyingTo />
                <SlashSkillMenu
                  items={slash.items}
                  active={slash.active}
                  onPick={slash.accept}
                />
              </>
            }
            onKeyDown={slash.onKeyDown}
            leading={<ChatFolderPicker folder={chatFolder} onChange={setChatFolder} />}
            sending={sending || (inFlight && !queueing)}
            onStop={inFlight && !stopping ? stop : undefined}
            placeholder={
              inFlight
                ? sendStatus.kind === "queue"
                  ? "Add a message; it waits for this turn"
                  : "Ask anything"
                : placeholderFor(status, "Ask anything")
            }
          />
        </div>
        {covered && <LoadingStatus overlay />}
      </div>
      {/* the placeholder already says why sending is off while the
          box is empty; the hint is only for when typed text hides it */}
      <SessionSubmissionStatus
        activityStatus={activityStatus}
        hint={
          status.kind === "disabled" && !inFlight && !covered && draft.trim() !== ""
            ? status.hint
            : null
        }
      />
    </div>
  );
}
