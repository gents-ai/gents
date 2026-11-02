import { cn } from "@gents/ui/lib/utils";
import { COMPOSER_FIELD } from "./composerField";
import { AttachmentTray, useAttachmentDraft } from "./attachments";
import { useMemo, useRef } from "react";
import { ComposerReasonBox, composerReason } from "./ComposerReason";
import { ChevronDown } from "lucide-react";
import { RichComposer } from "@gents/ui/conversation";
import { useApp, useView } from "@/app/AppContext";
import {
  useMailboxCause,
  useNodeCount,
  useSelectedNode,
  useSelectedNodeDid,
} from "@/hooks/useClient";
import { href } from "@/lib/router";
import { NodeSwitcher } from "@/app/NodeSwitcher";
import { placeholderFor } from "@/lib/send-status";
import { agentOf } from "@/lib/agents";
import { NodeAvatar, nodeAvatarOf } from "../AgentAvatar";
import { NodeHoverCard } from "../HoverCards";
import { AgentPicker } from "../BehaviorPicker";
import { agentName } from "../behavior";
import { SelectedChatFolder } from "../ChatFolderPicker";
import { AccessSentence } from "../parts";
import { ReplyingTo } from "../ReplyingTo";
import { useMentionMenu } from "../useMentionMenu";
import { useSlashMenu } from "../useSlashMenu";
import { GoalChip, goalCommand, useGoalMode } from "./goalMode";
import {
  ComposerFooter,
  SessionSubmissionStatus,
  presentedComposerSendStatus,
} from "./composerParts";
import { useAgentChoice, useComposerSend } from "./useComposerSend";
import { GoalDeliveryFeedback } from "./GoalDeliveryFeedback";
import { ChatDeliveryFeedback } from "./ChatDeliveryFeedback";
import { useDraftField } from "../../../hooks/draftStore";

export function NewSession() {
  const mailboxCause = useMailboxCause();
  const selectedNodeDid = useSelectedNodeDid();
  const nodeCount = useNodeCount();
  const deployment = useSelectedNode();
  const choice = useAgentChoice();
  const nodeName = deployment?.node.displayName ?? deployment?.label ?? "the agent";
  const env = deployment?.agentEnvironments.find((e) => e.agentId === choice.agentId);
  const chosenName = agentName(choice.agentId, deployment);
  return (
    <div
      key={selectedNodeDid ?? "new"}
      data-testid="session-screen"
      className="mx-auto grid min-h-full max-w-2xl content-center gap-6 px-6 py-16 animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none"
    >
      <div className="flex items-start gap-3">
        {deployment ? (
          /* the node's card, as on every other node avatar */
          <NodeHoverCard deployment={deployment} side="bottom">
            <span className="block shrink-0 rounded-full">
              <NodeAvatar
                name={nodeName}
                {...nodeAvatarOf(deployment)}
                className="size-8"
              />
            </span>
          </NodeHoverCard>
        ) : (
          <NodeAvatar name={nodeName} className="size-8" />
        )}
        <div>
          <h1 className="font-heading text-lg font-medium text-heading">
            Start a new chat with{" "}
            {selectedNodeDid && nodeCount > 1 ? (
              <NodeSwitcher
                route={{ name: "session", sessionId: null }}
                current={selectedNodeDid}
                trigger={
                  <button
                    type="button"
                    title="Choose a node"
                    className="inline-flex cursor-pointer items-center gap-1 rounded-sm underline decoration-border underline-offset-4 outline-none hover:decoration-foreground focus-visible:ring-2 focus-visible:ring-ring"
                  />
                }
              >
                {nodeName}
                <ChevronDown aria-hidden className="size-4 opacity-50" />
              </NodeSwitcher>
            ) : (
              nodeName
            )}
          </h1>
          <p className="mt-1 text-sm text-muted-foreground">
            {mailboxCause
              ? "Answering a mailbox item; the first message starts its request"
              : "The first message creates the session automatically"}
          </p>
        </div>
      </div>
      <NewSessionComposer />
      <p className="text-xs text-muted-foreground">
        <AccessSentence name={chosenName} env={env} />
        {deployment && choice.agentId && (
          <>
            {" "}
            <a
              href={href({
                name: "agent",
                nodeDid: deployment.nodeDid,
                section: "agents",
                item: choice.agentId,
              })}
              className="underline decoration-border underline-offset-4 hover:text-foreground"
            >
              Configure
            </a>
          </>
        )}
      </p>
    </div>
  );
}

function NewSessionComposer() {
  const { drafts, stores } = useApp();
  const draftKey = useView((view) => view.draftKey);
  const [field, setField] = useDraftField(drafts, draftKey);
  const activityStatus = useView((view) => view.shellProjection.activityStatus);
  const sendStatus = useView((view) => view.shellProjection.nonEmptyContentSendStatus);
  const sending = stores.chat.use.sending();
  const deployment = useSelectedNode();
  const choice = useAgentChoice();
  const {
    draft,
    setDraft,
    send,
    setGoal,
    error: sendError,
  } = useComposerSend(choice.agentId);
  const attachmentRoot = useRef<HTMLDivElement | null>(null);
  const textarea = useRef<HTMLDivElement | null>(null);
  const attachments = useAttachmentDraft(
    attachmentRoot,
    sendStatus.kind === "disabled",
    textarea,
  );
  const agent = agentOf(deployment, choice.agentId);
  /* what nothing typed could get past stands in the input's place */
  const reason = composerReason(deployment, choice.agentId, sendStatus);
  const picker = (
    <AgentPicker
      deployment={deployment}
      agentId={choice.agentId}
      onChange={choice.setPicked}
    />
  );
  const goalMode = useGoalMode({ draft, setDraft, onSet: setGoal });
  const slash = useSlashMenu(
    deployment?.skills ?? [],
    deployment?.contexts.find((c) => c.context_id === agent?.contextId),
    [goalCommand(null, false)],
    goalMode.create,
  );
  const mention = useMentionMenu(deployment?.nodeDid ?? null, null);
  const chipKinds = useMemo(
    () => ({ ...slash.chipKinds, ...mention.chipKinds }),
    [slash.chipKinds, mention.chipKinds],
  );
  return (
    <div
      data-testid="composer"
      ref={attachmentRoot}
      {...attachments.dropProps}
      className={cn("relative", attachments.over && "rounded-2xl ring-2 ring-ring")}
    >
      <input
        {...attachments.fileInputProps}
        className="hidden"
        aria-label="Choose attachments"
      />
      <div className="grid">
        <div
          inert={reason ? true : undefined}
          className={cn("[grid-area:1/1]", reason && "invisible")}
        >
          <RichComposer
            key={draftKey}
            editorState={field}
            onEditorStateChange={setField}
            className="text-foreground"
            fieldProps={COMPOSER_FIELD}
            value={draft}
            onChange={setDraft}
            onSend={
              goalMode.mode
                ? (text) => void goalMode.submit(text)
                : (text) => send(text, undefined, undefined, slash.prepare(text))
            }
            models={[]}
            fieldRef={textarea}
            commands={slash.names}
            composerRef={slash.field}
            commandsAt="word"
            chipKinds={chipKinds}
            triggers={goalMode.mode ? undefined : [slash.trigger, mention.trigger]}
            onAttach={attachments.pick}
            hasAttachments={attachments.hasAttachments}
            submitDisabled={attachments.pending}
            onPaste={attachments.onPaste}
            disabled={sendStatus.kind === "disabled"}
            above={
              goalMode.mode ? (
                <GoalChip editing={false} onCancel={goalMode.leave} />
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
            onKeyDown={(event) => {
              goalMode.onKeyDown(event);
            }}
            submitLabel={goalMode.mode ? "Set goal" : "Send"}
            leading={reason ? null : picker}
            sending={sending}
            placeholder={
              goalMode.mode
                ? "Describe the goal"
                : placeholderFor(
                    presentedComposerSendStatus(
                      draft,
                      sendStatus,
                      attachments.hasAttachments,
                    ),
                    "Ask anything",
                  )
            }
          />
        </div>
        {reason && (
          <ComposerReasonBox reason={reason} deployment={deployment} leading={picker} />
        )}
      </div>
      <GoalDeliveryFeedback />
      <ChatDeliveryFeedback />
      {sendError && (
        <p role="alert" className="mt-2 text-xs text-destructive">
          {sendError}
        </p>
      )}
      <ComposerFooter
        folder={<SelectedChatFolder />}
        status={
          reason ? null : <SessionSubmissionStatus activityStatus={activityStatus} />
        }
      />
    </div>
  );
}
