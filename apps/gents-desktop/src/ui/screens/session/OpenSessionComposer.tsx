import { QuestionPanel } from "./QuestionPanel";
import { MorphProvider, useMorphCard } from "@/lib/morph";
import { COMPOSER_FIELD } from "./composerField";
import { AskedChip } from "./AskedChip";
import { useUnansweredQuestions } from "../../hooks/useSessionQuestions";
import { useSelectedSessionFields } from "../../hooks/useSelectedSession";
import { AttachmentTray, SentAttachments, useAttachmentDraft } from "./attachments";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type RefCallback,
} from "react";
import { useStore } from "zustand";
import { ArrowDown, Check, Pencil, X } from "lucide-react";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { ComposerReasonBox, composerReason } from "./ComposerReason";
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
import { useMentionMenu } from "../useMentionMenu";
import { useSlashMenu } from "../useSlashMenu";
import {
  ComposerFooter,
  SessionSubmissionStatus,
  presentedComposerSendStatus,
} from "./composerParts";
import { ComposerEditContext, lastEditable, useComposerEditing } from "./composerEdit";
import { sentMessages, useComposerHistory } from "./composerHistory";
import { selectedIn } from "@/hooks/useSelectedSession";
import { GoalChip, goalCommand, goalIsEditable, useGoalMode } from "./goalMode";
import { useComposerSend } from "./useComposerSend";
import { useFollowUp } from "@/preferences";
import { GoalDeliveryFeedback } from "./GoalDeliveryFeedback";
import { ChatDeliveryFeedback } from "./ChatDeliveryFeedback";
import { ActiveNodeLabel } from "@/app/ActiveNodeLabel";
import { chipSkills, withSkillLines } from "@/lib/skillLines";

/* the one card the composer box and the question panel share (morph.css) */
const CARD = "[view-transition-class:morph-card]";

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
  const agent = agentOf(deployment, agentId);
  const editSlash = useSlashMenu(
    deployment?.skills ?? [],
    deployment?.contexts.find((c) => c.context_id === agent?.contextId),
    [],
    () => {},
  );
  const edit = useComposerEditing({
    queue,
    onEdit,
    setDraft,
    send,
    textarea,
    commands: editSlash.names,
  });
  const editing = edit.editing;
  const editContext = useMemo(
    () => ({ editingDocId: editing?.docId ?? null, begin: edit.begin }),
    [editing?.docId, edit.begin],
  );
  const sessionId = stores.selection.use.sessionId();
  const nodeDid = stores.selection.use.nodeDid();
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
  const mention = useMentionMenu(deployment?.nodeDid ?? null, sessionId);
  const commands = editing ? editSlash : slash;
  const chipKinds = useMemo(
    () => ({ ...commands.chipKinds, ...mention.chipKinds }),
    [commands.chipKinds, mention.chipKinds],
  );
  const history = useComposerHistory({
    field: slash.field,
    sent: () =>
      sentMessages(
        selectedIn(stores.session.getState(), {
          selectedSessionId: sessionId,
          selectedNodeDid: nodeDid,
        })?.timelineItems ?? [],
      ),
    kinds: chipKinds,
  });

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
  /* what nothing typed could get past stands in the input's place */
  const reason =
    blocked && !covered ? composerReason(deployment, agentId, sendStatus) : null;
  /* an open question takes the composer's place until it is answered, or
     until the person chooses to write instead; a new question asks again */
  const questionScope = useSelectedSessionFields((session) =>
    session?.nodeDid
      ? { nodeDid: session.nodeDid, sessionId: session.sessionId }
      : null,
  );
  const questions = useUnansweredQuestions(questionScope);
  const questionsKey = questions.map((q) => q.itemId).join(" ");
  const [typingFor, setTypingFor] = useState<string | null>(null);
  /* a question that arrives while the person writes waits beside the
     composer, so it neither hides their draft nor reads their next key as a
     pick; the composer still holds the focus while this renders */
  const [askedKey, setAskedKey] = useState(questionsKey);
  if (askedKey !== questionsKey) {
    setAskedKey(questionsKey);
    const shown = askedKey !== "" && typingFor !== askedKey && !editing;
    const writing =
      draft.trim() !== "" || !!attachmentRoot.current?.contains(document.activeElement);
    if (questionsKey && !shown && writing) setTypingFor(questionsKey);
  }
  const asking = questions.length > 0 && typingFor !== questionsKey && !editing;
  /* the composer box and the question panel are one card, so a question
     arriving or leaving morphs one into the other */
  const card = useMorphCard();
  const [shownAsking, setShownAsking] = useState(asking);
  useEffect(() => {
    if (shownAsking !== asking) card.morph(() => setShownAsking(asking));
  }, [asking, shownAsking, card]);
  const cardPart = card.part("card");
  const composerRef = useCallback(
    (element: HTMLDivElement | null) => {
      attachmentRoot.current = element;
      return cardPart(element);
    },
    [cardPart],
  );
  const draftKey = useView((view) => view.draftKey);
  const [field, setField] = useDraftField(drafts, draftKey);
  const leaveHistory = history.leave;
  useEffect(() => leaveHistory(), [draftKey, !!editing, leaveHistory]);
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
    const empty = draft === "" && !attachments.hasAttachments;
    if (event.key === "ArrowUp" && empty) {
      const last = lastEditable(queue);
      if (last) {
        event.preventDefault();
        edit.begin(last);
        return;
      }
    }
    if (history.onKeyDown(event, empty)) return;
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
          "pointer-events-none absolute inset-x-0 bottom-full h-8 bg-linear-to-t/srgb from-background to-transparent transition-opacity duration-200",
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
      {shownAsking && (
        <div ref={cardPart} className={CARD}>
          <MorphProvider value={card}>
            <QuestionPanel
              items={questions}
              onTypeInstead={() => setTypingFor(questionsKey)}
            />
          </MorphProvider>
        </div>
      )}
      {!shownAsking && questions.length > 0 && (
        <AskedChip item={questions[0]!} onAnswer={() => setTypingFor(null)} />
      )}
      <div
        data-testid="composer"
        hidden={shownAsking || undefined}
        ref={composerRef}
        {...activeAttachments.dropProps}
        className={cn(
          "relative grid rounded-2xl",
          CARD,
          activeAttachments.over && "ring-2 ring-ring",
        )}
      >
        <input
          {...activeAttachments.fileInputProps}
          className="hidden"
          aria-label="Choose attachments"
        />
        <div
          inert={covered || (reason && !editing) ? true : undefined}
          className={cn(
            "[grid-area:1/1]",
            (covered || (reason && !editing)) && "invisible",
          )}
        >
          <RichComposer
            key={editing ? `edit ${editing.docId}` : draftKey}
            editorState={editing ? null : field}
            onEditorStateChange={editing ? undefined : setField}
            className="text-foreground"
            fieldProps={COMPOSER_FIELD}
            value={editing ? editing.text : draft}
            onChange={editing ? edit.setText : setDraft}
            onSend={
              editing
                ? (text) => {
                    const skills = [
                      ...new Set([
                        ...editing.skills,
                        ...chipSkills(
                          editSlash.field.current?.parts() ?? [],
                          editSlash.names,
                        ),
                      ]),
                    ];
                    const prepared = withSkillLines(text, skills);
                    void (editing.reached
                      ? edit.sendAsNew(text, addedAttachments(), prepared)
                      : edit.save(text, addedAttachments(), prepared, skills));
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
            /* what the field counts as something to send besides its text:
               a queued message's kept skills are, as core takes a message of
               skills alone */
            hasAttachments={
              editing
                ? (editing.attachments?.length ?? 0) >
                    editing.removedAttachments.length ||
                  editing.capturedContext.length > 0 ||
                  editing.skills.length > 0 ||
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
                  {history.position && (
                    <p
                      role="status"
                      data-testid="composer-history"
                      className="px-3 pt-2 text-xs text-muted-foreground"
                    >
                      History {history.position.index}/{history.position.total}
                    </p>
                  )}
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
            onPartsChange={editing ? undefined : history.onPartsChange}
            fieldRef={textarea}
            commands={commands.names}
            composerRef={commands.field}
            commandsAt="word"
            chipKinds={chipKinds}
            triggers={goalMode.mode ? undefined : [commands.trigger, mention.trigger]}
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
        {reason && !editing && (
          <ComposerReasonBox reason={reason} deployment={deployment} />
        )}
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
          reason ? null : (
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
          )
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
