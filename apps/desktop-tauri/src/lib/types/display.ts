export function displayAgentIdentity(value?: string | null) {
  if (!value) {
    return null;
  }
  return value;
}

export function displayBehaviorLabel(value?: string | null) {
  if (!value || value === "default") {
    return null;
  }
  return value;
}

export function displayConversationTitle(value?: string | null) {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : "untitled";
}

export function displayGraphqlEndpoint(value?: string | null) {
  const trimmed = value?.trim();
  if (!trimmed) {
    return null;
  }
  try {
    const url = new URL(trimmed);
    return `${url.host}${url.pathname.replace(/\/$/, "")}`;
  } catch {
    return trimmed;
  }
}

export function formatBytes(value: number) {
  if (value < 1024) {
    return `${value} B`;
  }
  if (value < 1024 * 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}
