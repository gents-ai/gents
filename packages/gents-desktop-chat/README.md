# @source-inc/gents-desktop-chat

Chat workflow projection (`projectChatShell`, turn/send state), `useMasterDetail`,
composer, transcript, cancellation, tool rendering, and semantic styles.

```ts
import { ChatComposer, ChatTranscriptPanel } from "@source-inc/gents-desktop-chat";
import "@source-inc/gents-desktop-chat/styles.css";
```

**Required bridge grants:** default + session-read + chat-write + resend-control + interrupt-read + interrupt-control.
