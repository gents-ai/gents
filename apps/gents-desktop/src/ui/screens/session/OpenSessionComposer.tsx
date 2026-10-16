/* The foot of an open session: what the person reads and does next, over
   the composer (the goal, why sending waits, the messages queued behind the
   turn) and the line under it. It holds the draft, so a keystroke renders
   this and not the transcript above it. */
import { useMemo, useRef, type KeyboardEvent, type RefCallback } from "react";
import { ArrowDown, Check, Pencil, X } from "lucide-react";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Composer } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { useApp, useView } from "@/app/AppContext";
import { useInterruptVisible, useSelectedNode } from "@/hooks/useClient";
import { agentOf } from "@/lib/agents";
import { placeholderFor } from "@/lib/send-status";
import { SelectedChatFolder } from "../ChatFolderPicker";
import { LoadingStatus } from "../LoadingStatus";
import { SelectedPendingTray, useSessionQueue } from "../PendingTray";
import { ReplyingTo } from "../ReplyingTo";
import { Goal } from "../SessionHeader";
import { SlashSkillMenu } from "../SlashSkillMenu";
import { useSlashSkills } from "../useSlashSkills";
import {
  ComposerFooter,
  SessionSubmissionStatus,
  presentedComposerSendStatus,
} from "./composerParts";
import { ComposerEditContext, lastEditable, useComposerEditing } from "./composerEdit";
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
  const { stores } = useApp();
  const activityStatus = useView((view) => view.shellProjection.activityStatus);
  const sendStatus = useView((view) => view.shellProjection.nonEmptyContentSendStatus);
  const loadingStatus = useView((view) => view.loadingStatus);
  const sending = stores.chat.use.sending();
  const inFlight = useInterruptVisible();
  const deployment = useSelectedNode();
  const { draft, setDraft, send } = useComposerSend(agentId);
  const textarea = useRef<HTMLTextAreaElement | null>(null);
  const { queue, onEdit } = useSessionQueue();
  const edit = useComposerEditing({ queue, onEdit, setDraft, send, textarea });
  const editing = edit.editing;
  const editContext = useMemo(
    () => ({ editingDocId: editing?.docId ?? null, begin: edit.begin }),
    [editing?.docId, edit.begin],
  );
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

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (editing) {
      if (event.key === "Escape") {
        event.preventDefault();
        edit.cancel();
      }
      /* the message is already with the agent: a key meant to save must
         not send the text as a new message */
      if (
        editing.reached &&
        event.key === "Enter" &&
        !event.shiftKey &&
        !event.nativeEvent.isComposing
      )
        event.preventDefault();
      return;
    }
    /* ↑ in an empty composer edits the last message still waiting */
    if (event.key === "ArrowUp" && draft === "") {
      const last = lastEditable(queue);
      if (last) {
        event.preventDefault();
        edit.begin(last);
        return;
      }
    }
    slash.onKeyDown(event);
  };

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
      <ComposerEditContext.Provider value={editContext}>
        <SelectedPendingTray />
      </ComposerEditContext.Provider>
      {/* the card that covers the composer shares its cell, so a
          card taller than the composer grows the cell, not clips */}
      <div data-testid="composer" className="grid">
        <div
          inert={covered || undefined}
          className={cn("[grid-area:1/1]", covered && "invisible")}
        >
          {/* one composer either way, so the caret stays where it is */}
          <Composer
            /* the text wraps; at page zoom WebKit sizes a self-sizing textarea's
               content wider than its box, and would draw a horizontal bar */
            className="[&_textarea]:overflow-x-hidden"
            value={editing ? editing.text : draft}
            onChange={editing ? edit.setText : setDraft}
            onSend={
              editing
                ? (text) =>
                    void (editing.reached ? edit.sendAsNew(text) : edit.save(text))
                : send
            }
            models={[]}
            disabled={!editing && blocked}
            above={
              editing ? (
                <EditingChip reached={editing.reached} onCancel={edit.cancel} />
              ) : (
                <>
                  <ReplyingTo />
                  <SlashSkillMenu
                    items={slash.items}
                    active={slash.active}
                    onPick={slash.accept}
                  />
                </>
              )
            }
            onKeyDown={onKeyDown}
            textareaRef={textarea}
            /* while it holds an edit, the chip's X is the way out and the
               button saves; an edit the agent already took sends as new */
            clearable={!editing}
            submitLabel={
              editing ? (editing.reached ? "Send as a new message" : "Save") : "Send"
            }
            submitIcon={editing && !editing.reached ? <Check /> : undefined}
            sending={editing ? edit.saving : sending || (inFlight && !queueing)}
            onStop={!editing && inFlight && !stopping ? stop : undefined}
            placeholder={
              editing
                ? "Edit the message"
                : inFlight
                  ? "Ask anything"
                  : placeholderFor(status, "Ask anything")
            }
          />
        </div>
        {covered && <LoadingStatus overlay />}
      </div>
      {/* the placeholder already says why sending is off while the
          box is empty; the hint is only for when typed text hides it */}
      <ComposerFooter
        folder={<SelectedChatFolder />}
        status={
          <SessionSubmissionStatus
            activityStatus={activityStatus}
            hint={
              status.kind === "disabled" && !inFlight && !covered && draft.trim() !== ""
                ? status.hint
                : null
            }
          />
        }
      />
    </div>
  );
}

/* says the composer holds a waiting message, not a new one; Esc or the X
   puts it back unchanged */
function EditingChip({
  reached,
  onCancel,
}: {
  reached: boolean;
  onCancel: () => void;
}) {
  return (
    <div
      className="flex items-center gap-2 px-2 pt-2"
      data-testid="editing-message"
      data-reached={reached || undefined}
    >
      <span className="inline-flex h-6 items-center gap-1.5 rounded-full border border-border bg-background pr-1 pl-2 text-xs text-muted-foreground">
        <Pencil className="size-3 shrink-0" />
        <span className="text-foreground">
          {reached ? "Already reached the agent" : "Editing message"}
        </span>
        <button
          type="button"
          aria-label={reached ? "Keep as a draft" : "Cancel editing"}
          title={reached ? "Keep as a draft (Esc)" : "Cancel (Esc)"}
          onClick={onCancel}
          className="grid size-4 cursor-pointer place-items-center rounded-full hover:bg-accent hover:text-foreground"
        >
          <X className="size-3" />
        </button>
      </span>
      {reached && (
        <span className="text-xs text-muted-foreground">
          Send it as a new message, or keep it as a draft
        </span>
      )}
    </div>
  );
}
