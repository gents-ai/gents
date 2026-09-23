/* A behavior that was just created opens with its prompt in view, expanded
   and focused. The request is read when its editor mounts and cleared once
   acted on. PROPOSED screen. */
let pending: string | null = null;

export function requestPromptFocus(behaviorId: string) {
  pending = behaviorId;
}

export function promptFocusRequested(behaviorId: string) {
  return pending === behaviorId;
}

export function clearPromptFocus() {
  pending = null;
}
