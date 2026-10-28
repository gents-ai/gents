import { AttachmentTray, SentAttachments, useAttachmentDraft } from "./attachments";
import {
  useEffect,
  useMemo,
  useRef,
  type KeyboardEvent,
  type RefCallback,
} from "react";
import { useStore } from "zustand";
import { ArrowDown, Check, Pencil, X } from "lucide-react";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { RichComposer } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { useApp, useView } from "@/app/AppContext";
import { useInterruptVisible, useSelectedNode } from "@/hooks/useClient";
import { focusDraft, useDraftField, writeAttachments } from "../../../hooks/draftStore";
import { agentOf } from "@/lib/agents";
import { placeholderFor } from "@/lib/send-status";
import { SelectedChatFolder } from "../ChatFolderPicker";
import { LoadingStatus } from "../LoadingStatus";
import { SelectedPendingTray, useSessionQueue } from "../PendingTray";
import { ReplyingTo } from "../ReplyingTo";
import { Goal } from "../SessionHeader";
import { useSlashMenu } from "../useSlashMenu";
import {
  ComposerFooter,
  SessionSubmissionStatus,
  presentedComposerSendStatus,
} from "./composerParts";
import { ComposerEditContext, lastEditable, useComposerEditing } from "./composerEdit";
import { GoalChip, goalCommand, goalIsEditable, useGoalMode } from "./goalMode";
import { useComposerSend } from "./useComposerSend";
import { useFollowUp } from "@/preferences";
import { GoalDeliveryFeedback } from "./GoalDeliveryFeedback";
import { ChatDeliveryFeedback } from "./ChatDeliveryFeedback";
import { ActiveNodeLabel } from "@/app/ActiveNodeLabel";
import { chipSkills, withSkillLines } from "@/lib/skillLines";

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
  const { drafts, stores, actions } = useApp();
  const activityStatus = useView((view) => view.shellProjection.activityStatus);
  const sendStatus = useView((view) => view.shellProjection.nonEmptyContentSendStatus);
  const loadingStatus = useView((view) => view.loadingStatus);
  const sending = stores.chat.use.sending();
  const inFlight = useInterruptVisible();
  const deployment = useSelectedNode();
  const { draft, setDraft, send, setGoal, error: sendError } = useComposerSend(agentId);
  const textarea = useRef<HTMLDivElement | null>(null);
  const attachmentRoot = useRef<HTMLDivElement | null>(null);
  const { queue, onEdit } = useSessionQueue();
  const edit = useComposerEditing({ queue, onEdit, setDraft, send, textarea });
  const editing = edit.editing;
  const editContext = useMemo(
    () => ({ editingDocId: editing?.docId ?? null, begin: edit.begin }),
    [editing?.docId, edit.begin],
  );
  const agent = agentOf(deployment, agentId);
  const sessionId = stores.selection.use.sessionId();
  const goalMode = useGoalMode({
    draft,
    setDraft,
    onSet: setGoal,
    onEdit: sessionId
      ? (objective) =>
          actions.controlGoal(sessionId, { kind: "edit", objective, tokenBudget: null })
      : undefined,
  });
  const slash = useSlashMenu(
    deployment?.skills ?? [],
    deployment?.contexts.find((c) => c.context_id === agent?.contextId),
    [goalCommand(goal, inFlight)],
    () =>
      goal && goalIsEditable(goal)
        ? goalMode.edit(goal.objective ?? "")
        : goalMode.create(),
  );
  const editSlash = useSlashMenu(
    deployment?.skills ?? [],
    deployment?.contexts.find((c) => c.context_id === agent?.contextId),
    [],
    () => {},
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
  const draftKey = useView((view) => view.draftKey);
  const [field, setField] = useDraftField(drafts, draftKey);
  const focusRequested = useStore(drafts, (state) => state.focus === draftKey);
  useEffect(() => {
    if (!focusRequested || blocked) return;
    focusDraft(drafts, null);
    textarea.current?.focus();
  }, [focusRequested, blocked, drafts]);

  const attachments = useAttachmentDraft(
    attachmentRoot,
    blocked || readOnly !== null || !!editing,
    textarea,
  );
  const editAttachmentKey = `${draftKey}:queued-edit:${editing?.docId ?? "none"}`;
  const editAttachments = useAttachmentDraft(
    attachmentRoot,
    !editing || edit.saving || blocked || readOnly !== null,
    textarea,
    editAttachmentKey,
  );
  useEffect(() => {
    if (!editing) return;
    return () => writeAttachments(drafts, editAttachmentKey, () => []);
  }, [drafts, editAttachmentKey, !!editing]);
  const activeAttachments = editing ? editAttachments : attachments;
  const editAttachmentLimit =
    editing &&
    (editing.attachments?.length ?? 0) -
      editing.removedAttachments.length +
      editAttachments.items.filter((item) => !item.error).length >
      4;
  const addedAttachments = () =>
    editAttachments.items.flatMap((item) => (item.content ? [item.content] : []));
  const cancelEdit = () => void edit.cancel(addedAttachments());

  const status = presentedComposerSendStatus(
    draft,
    sendStatus,
    attachments.hasAttachments,
  );
  const queueing = status.kind === "queue";

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (editing) {
      if (event.key === "Escape") {
        event.preventDefault();
        cancelEdit();
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
    goalMode.onKeyDown(event);
    if (event.defaultPrevented || goalMode.mode) return;
    if (event.key === "ArrowUp" && draft === "" && !attachments.hasAttachments) {
      const last = lastEditable(queue);
      if (last) {
        event.preventDefault();
        edit.begin(last);
        return;
      }
    }
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
      void send(draft, undefined, otherWay, slash.prepare(draft));
    }
  };

  return (
    <div ref={roomRef} className="sticky bottom-0 z-20 mt-auto bg-background pt-6 pb-6">
      {goal && (
        <Goal
          goal={goal}
          onEdit={() => {
            goalMode.edit(goal.objective ?? "");
            textarea.current?.focus();
          }}
          onControl={
            sessionId
              ? (action) => void actions.controlGoal(sessionId, action).catch(() => {})
              : undefined
          }
        />
      )}
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
      <GoalDeliveryFeedback />
      <ChatDeliveryFeedback />
      <div
        data-testid="composer"
        ref={attachmentRoot}
        {...activeAttachments.dropProps}
        className={cn(
          "relative grid rounded-2xl",
          activeAttachments.over && "ring-2 ring-ring",
        )}
      >
        <input
          {...activeAttachments.fileInputProps}
          className="hidden"
          aria-label="Choose attachments"
        />
        <div
          inert={covered || undefined}
          className={cn("[grid-area:1/1]", covered && "invisible")}
        >
          <RichComposer
            key={editing ? `edit ${editing.docId}` : draftKey}
            editorState={editing ? null : field}
            onEditorStateChange={editing ? undefined : setField}
            className="text-foreground"
            value={editing ? editing.text : draft}
            onChange={editing ? edit.setText : setDraft}
            onSend={
              editing
                ? (text) => {
                    const prepared = withSkillLines(text, [
                      ...new Set([
                        ...editing.skills,
                        ...chipSkills(
                          editSlash.field.current?.parts() ?? [],
                          editSlash.names,
                        ),
                      ]),
                    ]);
                    void (editing.reached
                      ? edit.sendAsNew(text, addedAttachments(), prepared)
                      : edit.save(text, addedAttachments(), prepared));
                  }
                : goalMode.mode
                  ? (text) => void goalMode.submit(text)
                  : (text) => send(text, undefined, followUp, slash.prepare(text))
            }
            models={[]}
            onAttach={activeAttachments.pick}
            attachDisabled={
              blocked || readOnly !== null || (editing && edit.saving) || false
            }
            hasAttachments={
              editing
                ? (editing.attachments?.length ?? 0) >
                    editing.removedAttachments.length ||
                  editing.capturedContext.length > 0 ||
                  editAttachments.hasAttachments
                : attachments.hasAttachments
            }
            submitDisabled={activeAttachments.pending || !!editAttachmentLimit}
            onPaste={activeAttachments.onPaste}
            disabled={!editing && blocked}
            above={
              editing ? (
                <>
                  <EditingChip reached={editing.reached} onCancel={cancelEdit} />
                  <SentAttachments
                    items={editing.attachments}
                    requestId={editing.requestId}
                    removed={editing.removedAttachments}
                    onRemove={edit.removeAttachment}
                  />
                  <AttachmentTray
                    items={editAttachments.items}
                    remove={editAttachments.remove}
                    error={
                      editAttachmentLimit
                        ? "Attach up to 4 files"
                        : editAttachments.error
                    }
                  />
                  <div className="flex flex-wrap gap-2 px-3 pt-2">
                    {editing.skills.map((skill) => (
                      <Button
                        key={skill}
                        type="button"
                        size="sm"
                        variant="outline"
                        onClick={() => edit.removeSkill(skill)}
                        aria-label={`Remove skill ${skill}`}
                      >
                        /{skill}
                        <X />
                      </Button>
                    ))}
                    {editing.capturedContext.map((capture, index) => (
                      <Button
                        key={`${index}:${capture.uri ?? capture.kind}`}
                        type="button"
                        size="sm"
                        variant="outline"
                        onClick={() => edit.removeCapture(index)}
                        aria-label={`Remove context ${capture.uri ?? capture.terminalName ?? capture.kind}`}
                      >
                        {capture.uri ?? capture.terminalName ?? capture.kind}
                        <X />
                      </Button>
                    ))}
                  </div>
                </>
              ) : goalMode.mode ? (
                <GoalChip
                  editing={goalMode.mode === "edit"}
                  onCancel={goalMode.leave}
                />
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
                </>
              )
            }
            onKeyDown={onKeyDown}
            fieldRef={textarea}
            commands={editing ? editSlash.names : slash.names}
            composerRef={editing ? editSlash.field : slash.field}
            commandsAt="word"
            chipKinds={editing ? editSlash.chipKinds : slash.chipKinds}
            triggers={
              goalMode.mode ? undefined : [editing ? editSlash.trigger : slash.trigger]
            }
            clearable={!editing}
            submitLabel={
              editing
                ? editing.reached
                  ? "Send as a new message"
                  : "Save"
                : goalMode.mode === "edit"
                  ? "Save goal"
                  : goalMode.mode
                    ? "Set goal"
                    : "Send"
            }
            submitIcon={editing && !editing.reached ? <Check /> : undefined}
            sending={editing ? edit.saving : sending || (inFlight && !queueing)}
            onStop={!editing && inFlight && !stopping ? stop : undefined}
            placeholder={
              editing
                ? "Edit the message"
                : goalMode.mode
                  ? "Describe the goal"
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
        folder={
          <>
            <ActiveNodeLabel composer />
            <SelectedChatFolder />
          </>
        }
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
