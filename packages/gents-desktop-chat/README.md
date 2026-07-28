# @source-inc/gents-desktop-chat

Chat workflow projection (`projectChatShell`, turn/send state), `useMasterDetail`,
composer, transcript, cancellation, tool rendering, and semantic styles.

```ts
import {
  ChatComposer,
  ChatTranscriptPanel,
} from "@source-inc/gents-desktop-chat";
```

```css
@import "@source-inc/gents-desktop-tokens/semantic.css";
@import "@source-inc/gents-desktop-ui/styles.css";
@import "@source-inc/gents-desktop-chat/styles.css";
/* Host semantic-token overrides come last. */
```

**Required bridge grants:** default + session-read + chat-write + resend-control + interrupt-read + interrupt-control.
