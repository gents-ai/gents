/* Starting a new session: the node and agent it starts with, and the
   composer whose first message creates it. */
import { Fragment } from "react";
import { ChevronDown } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Composer } from "@gents/ui/conversation";
import { useApp, useView } from "@/app/AppContext";
import {
  useChatFolder,
  useHomeDid,
  useMailboxCause,
  useNodeCount,
  useSelectedNode,
  useSelectedNodeDid,
} from "@/hooks/useClient";
import { href } from "@/lib/router";
import { placeholderFor } from "@/lib/send-status";
import { isWorkingNode } from "@/lib/nodes";
import { fleetNodes } from "@/lib/scope";
import { agentOf } from "@/lib/agents";
import { useFleet } from "../../hooks/useFleet";
import { NodeAvatar } from "../AgentAvatar";
import { AgentPicker } from "../BehaviorPicker";
import { agentName } from "../behavior";
import { ChatFolderPicker } from "../ChatFolderPicker";
import { AccessSentence } from "../parts";
import { ReplyingTo } from "../ReplyingTo";
import { SlashSkillMenu } from "../SlashSkillMenu";
import { useSlashSkills } from "../useSlashSkills";
import { SessionSubmissionStatus, presentedComposerSendStatus } from "./composerParts";
import { useAgentChoice, useComposerSend } from "./useComposerSend";

export function NewSession() {
  const { selectNode } = useApp().actions;
  const nodeCount = useNodeCount();
  const mailboxCause = useMailboxCause();
  const selectedNodeDid = useSelectedNodeDid();
  const deployment = useSelectedNode();
  const choice = useAgentChoice();
  /* the node's name, or the pairing label while a paired node has not
     replicated its configuration yet */
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
        <NodeAvatar name={nodeName} className="size-8" />
        <div>
          <h1 className="font-heading text-lg font-medium text-heading">
            Start a new chat with{" "}
            {nodeCount > 1 ? (
              <NodeChoice
                name={nodeName}
                selectedNodeDid={selectedNodeDid}
                onSelect={selectNode}
              />
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
  const {
    stores,
    actions: { setChatFolder },
  } = useApp();
  const chatFolder = useChatFolder();
  const activityStatus = useView((view) => view.shellProjection.activityStatus);
  const sendStatus = useView((view) => view.shellProjection.nonEmptyContentSendStatus);
  const sending = stores.chat.use.sending();
  const deployment = useSelectedNode();
  const choice = useAgentChoice();
  const { draft, setDraft, send } = useComposerSend(choice.agentId);
  const agent = agentOf(deployment, choice.agentId);
  const slash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    deployment?.contexts.find((c) => c.context_id === agent?.contextId),
  );
  return (
    <div data-testid="composer">
      <Composer
        value={draft}
        onChange={setDraft}
        onSend={send}
        models={[]}
        disabled={sendStatus.kind === "disabled"}
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
        leading={
          <>
            <AgentPicker
              deployment={deployment}
              agentId={choice.agentId}
              onChange={choice.setPicked}
            />
            <ChatFolderPicker folder={chatFolder} onChange={setChatFolder} />
          </>
        }
        sending={sending}
        placeholder={placeholderFor(
          presentedComposerSendStatus(draft, sendStatus),
          "Ask anything",
        )}
      />
      {/* inside the composer's row, so the grid's gap is not paid twice
          around a line that is usually empty */}
      <SessionSubmissionStatus activityStatus={activityStatus} reserve={false} />
    </div>
  );
}

/* the node a new chat starts on, where there is a choice: the name is the
   button's own text, so the heading still reads as one sentence */
function NodeChoice({
  name,
  selectedNodeDid,
  onSelect,
}: {
  name: string;
  selectedNodeDid: string | null;
  onSelect: (nodeDid: string) => void;
}) {
  const nodes = useFleet(useShallow(fleetNodes));
  const homeDid = useHomeDid();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <button
            type="button"
            title="Choose a node"
            className="inline-flex items-center gap-1 underline decoration-border underline-offset-4 hover:decoration-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm"
          />
        }
      >
        {name}
        <ChevronDown className="size-4 opacity-50" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-56">
        <DropdownMenuRadioGroup value={selectedNodeDid ?? ""} onValueChange={onSelect}>
          {nodes.map((n, i, all) => (
            <Fragment key={n.nodeDid}>
              {/* a faint line between the local node and the paired ones */}
              {i > 0 &&
                isWorkingNode(all[i - 1]!, homeDid) &&
                !isWorkingNode(n, homeDid) && (
                  <DropdownMenuSeparator className="opacity-60" />
                )}
              <DropdownMenuRadioItem value={n.nodeDid} disabled={!n.dialSucceeded}>
                <NodeAvatar
                  name={n.node.displayName ?? n.label}
                  className="size-5 text-[9px]"
                />
                <span className="min-w-0 flex-1 truncate">
                  {n.node.displayName ?? n.label}
                </span>
                {isWorkingNode(n, homeDid) && (
                  <span className="text-xs text-muted-foreground">local</span>
                )}
              </DropdownMenuRadioItem>
            </Fragment>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
