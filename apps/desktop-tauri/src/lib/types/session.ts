export type MessageView = {
  messageKey: string;
  sequence?: number | null;
  role?: string | null;
  content?: string | null;
  displayRole?: string | null;
  displayContent?: string | null;
  reasoning?: string | null;
  hasToolCalls: boolean;
  hasToolResults: boolean;
  timestamp?: string | null;
};

export type ToolDetailFieldView = {
  key: string;
  value: string;
};

export type ToolDetailValueView = {
  rawText: string;
  fields: ToolDetailFieldView[];
};

export type RenderedToolCallView = {
  itemKey: string;
  toolName: string;
  status?: string | null;
  statusKind: string;
  args?: ToolDetailValueView | null;
  result?: ToolDetailValueView | null;
};

export type ResponseView = {
  status?: string | null;
  content?: string | null;
  reasoning?: string | null;
  errorMessage?: string | null;
  tokenCount?: number | null;
  materializedMessageSequence?: number | null;
  materializedAt?: string | null;
  completedAt?: string | null;
};

export type PendingTurnView = {
  requestId: string;
  content: string;
  lifecycleState?: string | null;
  createdAt?: string | null;
};

export type RenderedTimelineItem =
  | {
      kind: "userMessage";
      itemKey: string;
      sequence?: number | null;
      content: string;
    }
  | {
      kind: "assistantMessage";
      itemKey: string;
      sequence?: number | null;
      content?: string | null;
      reasoning?: string | null;
    }
  | {
      kind: "toolGroup";
      itemKey: string;
      messageSequence?: number | null;
      tools: RenderedToolCallView[];
    }
  | {
      kind: "pendingUserTurn";
      itemKey: string;
      requestId: string;
      content: string;
      lifecycleState?: string | null;
      createdAt?: string | null;
    }
  | {
      kind: "liveAssistant";
      itemKey: string;
      content?: string | null;
      reasoning?: string | null;
    };

export type DesktopSessionSnapshot = {
  sessionId: string;
  agentDid?: string | null;
  behaviorId?: string | null;
  title?: string | null;
  previewText?: string | null;
  status?: string | null;
  turnState?: string | null;
  latestRequestId?: string | null;
  latestResponse?: ResponseView | null;
  activeResponseOverlay?: ResponseView | null;
  pendingTurn?: PendingTurnView | null;
  timelineItems: RenderedTimelineItem[];
};

export type ChatSendResult = {
  sessionId: string;
  requestId: string;
  agentDid: string;
  behaviorId?: string | null;
};
