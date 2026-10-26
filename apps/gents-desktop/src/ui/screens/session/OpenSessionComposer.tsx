import { AttachmentTray, SentAttachments, useAttachmentDraft } from "./attachments";
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
import { useFollowUp } from "@/preferences";

export function OpenSessionComposer({
  roomRef,
  atBottom,
  toBottom,
  stop,
  stopping,
  goal,
  agentId,
}: {
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
  const { draft, setDraft, send, error: sendError } = useComposerSend(agentId);
  const textarea = useRef<HTMLTextAreaElement | null>(null);
  const attachmentRoot = useRef<HTMLDivElement | null>(null);
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

  /* a session that never takes this app's messages says so before anything is typed */
  const readOnly =
    sendStatus.kind === "disabled" && sendStatus.reason === "sessionNotOwned"
      ? sendStatus.hint
      : null;
  /* a message behind a running turn goes the way the setting says, and the
     modified Enter sends that one message the other way */
  const followUp = useFollowUp();
  const turnRunning = sendStatus.kind === "queue";
  const otherWay = followUp === "steer" ? "queue" : "steer";
  const blocked = sendStatus.kind === "disabled" && !inFlight;
  const covered = blocked && loadingStatus !== null;

  const attachments = useAttachmentDraft(
    attachmentRoot,
    blocked || readOnly !== null || !!editing,
    textarea,
  );

  const status = presentedComposerSendStatus(
    draft,
    sendStatus,
    attachments.hasAttachments,
  );
  const queueing = status.kind === "queue";

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (editing) {
      if (event.key === "Escape") {
        event.preventDefault();
        edit.cancel();
      }
      if (
        editing.reached &&
        event.key === "Enter" &&
        !event.shiftKey &&
        !event.nativeEvent.isComposing
      )
        event.preventDefault();
      return;
    }
    if (event.key === "ArrowUp" && draft === "" && !attachments.hasAttachments) {
      const last = lastEditable(queue);
      if (last) {
        event.preventDefault();
        edit.begin(last);
        return;
      }
    }
    slash.onKeyDown(event);
    if (
      !event.defaultPrevented &&
      turnRunning &&
      event.key === "Enter" &&
      (event.metaKey || event.ctrlKey) &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing &&
      draft.trim() !== ""
    ) {
      event.preventDefault();
      void send(draft, undefined, otherWay);
    }
  };

  return (
    <div ref={roomRef} className="sticky bottom-0 z-20 mt-auto bg-background pt-6 pb-6">
      {goal && <Goal goal={goal} />}
      <div
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-x-0 bottom-full h-8 bg-gradient-to-t from-background to-transparent transition-opacity duration-200",
          atBottom ? "opacity-0" : "opacity-100",
        )}
      />
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
      <div
        data-testid="composer"
        ref={attachmentRoot}
        {...attachments.dropProps}
        className={cn("grid rounded-2xl", attachments.over && "ring-2 ring-ring")}
      >
        <input
          {...attachments.fileInputProps}
          className="hidden"
          aria-label="Choose attachments"
        />
        <div
          inert={covered || undefined}
          className={cn("[grid-area:1/1]", covered && "invisible")}
        >
          <Composer
            className="text-foreground"
            value={editing ? editing.text : draft}
            onChange={editing ? edit.setText : setDraft}
            onSend={
              editing
                ? (text) =>
                    void (editing.reached ? edit.sendAsNew(text) : edit.save(text))
                : (text) => send(text, undefined, followUp)
            }
            models={[]}
            onAttach={attachments.pick}
            attachDisabled={blocked || readOnly !== null || !!editing}
            hasAttachments={
              editing ? !!editing.attachments?.length : attachments.hasAttachments
            }
            submitDisabled={!editing && attachments.pending}
            onPaste={attachments.onPaste}
            disabled={!editing && blocked}
            above={
              editing ? (
                <>
                  <EditingChip reached={editing.reached} onCancel={edit.cancel} />
                  <SentAttachments
                    items={editing.attachments}
                    requestId={editing.requestId}
                  />
                </>
              ) : (
                <>
                  <ReplyingTo />
                  {attachments.over && (
                    <p
                      role="status"
                      className="px-3 pt-2 text-xs text-muted-foreground"
                    >
                      Drop files to attach
                    </p>
                  )}
                  <AttachmentTray
                    items={attachments.items}
                    remove={attachments.remove}
                    error={attachments.error}
                  />
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
                  : readOnly
                    ? readOnly
                    : placeholderFor(status, "Ask anything")
            }
          />
        </div>
        {covered && <LoadingStatus overlay />}
      </div>
      {edit.error && (
        <p role="alert" className="mt-2 text-xs text-destructive">
          {edit.error}
        </p>
      )}
      {sendError && (
        <p role="alert" className="mt-2 text-xs text-destructive">
          {sendError}
        </p>
      )}
      <ComposerFooter
        folder={<SelectedChatFolder />}
        status={
          <SessionSubmissionStatus
            activityStatus={activityStatus}
            hint={
              status.kind === "disabled" &&
              !inFlight &&
              !covered &&
              (draft.trim() !== "" || attachments.hasAttachments)
                ? status.hint
                : null
            }
          />
        }
      />
    </div>
  );
}

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
