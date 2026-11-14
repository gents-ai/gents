export type SentTurn = {
  sessionId: string;
  requestId: string;
  queued: boolean;
  taken?: boolean;
};

export const turnKey = (requestId: string) => `turn:${requestId}`;
