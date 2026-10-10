/* Sessions and their timeline items as the bridge sends them: every field
   present, the ones a test does not care about null. A test names only
   what it is about. */
import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";

type Of<K extends RenderedTimelineItem["kind"]> = Extract<
  RenderedTimelineItem,
  { kind: K }
>;
type Given<K extends RenderedTimelineItem["kind"]> = Partial<Of<K>> & {
  itemKey: string;
};

const READY = { state: "ready" } as const;

export const userMessage = (item: Given<"userMessage">): Of<"userMessage"> => ({
  ownsTurn: true,
  sequence: null,
  content: null,
  timestamp: null,
  reconstruction: READY,
  ...item,
  kind: "userMessage",
});

export const assistantMessage = (
  item: Given<"assistantMessage">,
): Of<"assistantMessage"> => ({
  sequence: null,
  content: null,
  reasoning: null,
  timestamp: null,
  reconstruction: READY,
  ...item,
  kind: "assistantMessage",
});

export const liveAssistant = (item: Given<"liveAssistant">): Of<"liveAssistant"> => ({
  content: null,
  reasoning: null,
  ...item,
  kind: "liveAssistant",
});

export const pendingUserTurn = (
  item: Given<"pendingUserTurn"> & { requestId: string },
): Of<"pendingUserTurn"> => ({
  content: "",
  selectedSkillIds: [],
  lifecycleState: null,
  foldedIntoRequestId: null,
  origin: null,
  createdAt: null,
  ...item,
  kind: "pendingUserTurn",
});

/** A session's context: an empty window that nothing has compacted. */
export const sessionContext = (
  context: Partial<DesktopSessionSnapshot["context"]> = {},
): DesktopSessionSnapshot["context"] => ({
  estimatedDurableTokens: 0,
  estimatedConversationTokens: 0,
  contextWindow: 128_000,
  compactionThreshold: 0.8,
  compactionThresholdTokens: 102_400,
  compactionStrategy: "StripThenSummarize",
  durableMessageCount: 0,
  providerMessageCount: 0,
  totalCompactedMessages: 0,
  compactions: [],
  lastRequest: null,
  ...context,
});

/** A session as read: settled, with no turn in flight. */
export const sessionSnapshot = (
  session: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot => ({
  sessionId: "session-1",
  nodeDid: "did:test:node",
  agentId: null,
  title: null,
  previewText: null,
  status: null,
  goal: null,
  turnState: "completed",
  latestRequestId: null,
  retryEligibility: { eligible: false, denialReason: null },
  latestRequestOutcome: null,
  pendingTurn: null,
  queuedTurns: [],
  foldedInputs: [],
  context: sessionContext(),
  timelineItems: [],
  ...session,
});
