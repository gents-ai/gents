export function findAgentDeploymentControl(label: string): HTMLButtonElement | null {
  const expected = normalized(label);
  const expectedChatLabel = normalized(`Open ${label} chat`);
  const expectedDetailsTitle = normalized(`Open ${label} details`);
  const selectors = [
    '[data-testid^="fleet-chat-"], [aria-label^="Open "][aria-label$=" chat"]',
    '[data-testid^="fleet-detail-name-"]',
  ];
  for (const selector of selectors) {
    const match = Array.from(
      document.querySelectorAll<HTMLButtonElement>(selector),
    ).find(
      (button) =>
        normalized(button.textContent ?? "") === expected ||
        normalized(button.getAttribute("aria-label") ?? "") === expectedChatLabel ||
        normalized(button.getAttribute("title") ?? "") === expectedDetailsTitle,
    );
    if (match) {
      return match;
    }
  }
  return null;
}

export function findAgentChatButton(label: string): HTMLButtonElement | null {
  const expected = normalized(`Open ${label} chat`);
  return (
    Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        '[data-testid^="fleet-chat-"], [aria-label^="Open "][aria-label$=" chat"]',
      ),
    ).find(
      (button) =>
        !button.disabled &&
        normalized(button.getAttribute("aria-label") ?? "") === expected,
    ) ?? null
  );
}

export function findNewChatButton(label: string): HTMLButtonElement | null {
  const enabledCurrent = Array.from(
    document.querySelectorAll<HTMLButtonElement>('[data-testid^="sidebar-new-chat-"]'),
  ).filter((button) => !button.disabled);
  if (enabledCurrent.length === 1) {
    return enabledCurrent[0];
  }
  if (enabledCurrent.length > 1) {
    return null;
  }

  const expected = normalized(`Start new chat with ${label}`);
  return (
    Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        '[aria-label^="Start new chat with "]',
      ),
    ).find(
      (button) =>
        !button.disabled &&
        normalized(button.getAttribute("aria-label") ?? "") === expected,
    ) ?? null
  );
}

export function findAssistantResponseMarker(
  root: ParentNode,
  expectedResponse: string,
): HTMLElement | null {
  return (
    Array.from(
      root.querySelectorAll<HTMLElement>('[data-testid="assistant-message"]'),
    ).find((message) =>
      message
        .querySelector<HTMLElement>(".message-content")
        ?.textContent?.includes(expectedResponse),
    ) ?? null
  );
}

export function conversationRowCount(root: ParentNode): number {
  return root.querySelectorAll(
    '.conversation-list .conversation-row > button[data-testid^="conversation-"]',
  ).length;
}

export function isConversationTurnSettled(
  root: ParentNode,
  expectedResponse: string,
): boolean {
  return (
    findAssistantResponseMarker(root, expectedResponse) !== null &&
    root.querySelector('[data-testid="cancel-button"]') === null
  );
}

function normalized(value: string) {
  return value.trim().toLocaleLowerCase();
}
