import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect } from "vitest";

import App from "../src/App";
import { setDesktopShellTimingConfigForTests } from "../src/hooks/useDesktopShell";
import { navigate } from "../src/ui/lib/router";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { type DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";

export type TauriDriverChatRequest = {
  agentDid: string;
  behaviorId?: string | null;
  sessionId?: string | null;
  content: string;
};

export type TauriDriverBridge = {
  adapter: DesktopApiAdapter;
  listenerFactory: DesktopClientUpdatedListenerFactory;
  sentRequests: TauriDriverChatRequest[];
  sendResults?: Array<{ sessionId: string }>;
  dispose?: () => Promise<void> | void;
};

type TauriDriverTimingConfig = {
  p2pAutoRestartCooldownMs?: number;
  clientRestartMaxAttempts?: number;
  clientRestartBackoffMs?: number;
};

export function renderTauriAppDriverWithBridge(
  bridge: TauriDriverBridge,
  firstPeerId: string | null = null,
  timingConfig: TauriDriverTimingConfig | null = null,
) {
  setDesktopShellTimingConfigForTests(timingConfig);
  if (typeof globalThis.IntersectionObserver === "undefined") {
    globalThis.IntersectionObserver = class IntersectionObserver {
      readonly root = null;
      readonly rootMargin = "0px";
      readonly thresholds = [0];
      disconnect() {}
      observe() {}
      takeRecords() {
        return [];
      }
      unobserve() {}
    };
  }

  const user = userEvent.setup();
  const rendered = render(
    <App
      bridge={{
        api: bridge.adapter,
        listenToUpdates: bridge.listenerFactory,
      }}
    />,
  );

  return {
    bridge,
    user,
    composer() {
      return screen.getByRole("textbox", { name: "Message" }) as HTMLTextAreaElement;
    },
    sendButton() {
      return screen.getByRole("button", { name: "Send" });
    },
    session(sessionId: string) {
      return screen.getByTestId(`session-${sessionId}`);
    },
    configButton() {
      return screen.getAllByRole("link", { name: / configuration$/i })[0];
    },
    chatButton() {
      const sessionId =
        bridge.sendResults?.at(-1)?.sessionId ?? bridge.sentRequests.at(-1)?.sessionId;
      if (sessionId) {
        const existing = [...screen.getAllByRole("link")].find(
          (link) => link.getAttribute("href") === `/sessions/${sessionId}`,
        );
        if (existing) return existing;
      }
      const recent = [...screen.getAllByRole("link")].find((link) =>
        /^\/sessions\/(?!new$).+/.test(link.getAttribute("href") ?? ""),
      );
      if (recent) return recent;
      return screen.getAllByRole("link", { name: "New session" })[0];
    },
    configSectionTab(tabId: string) {
      const labels: Record<string, string> = {
        agent: "Agent",
        behaviors: "Behaviours",
        contexts: "Contexts",
        skills: "Skills",
        inference: "Backends",
        profiles: "Profiles",
        tools: "Tools",
        "tool-services": "Remote Tools",
        tasks: "Tasks",
        schedules: "Schedules",
        "event-sources": "Event sources",
        triggers: "Triggers",
      };
      const label = labels[tabId] ?? tabId;
      return screen.getByRole("link", { name: new RegExp(`^${label}(?:\\s+\\d+)?$`) });
    },
    behaviorKey() {
      return screen.getByTestId("behavior-id") as HTMLInputElement;
    },
    behaviorSystemPrompt() {
      return screen.getByTestId("behavior-system-prompt") as HTMLTextAreaElement;
    },
    behaviorSaveButton() {
      return screen.getByTestId("behavior-save");
    },
    behaviorSaveStatus() {
      return screen.getByText("Saved", { selector: ".config-editor .chip" });
    },
    contextSystemPrompt() {
      return screen.getByRole("textbox", {
        name: "System prompt",
      }) as HTMLTextAreaElement;
    },
    input(testId: string) {
      return screen.getByTestId(testId) as HTMLInputElement;
    },
    textarea(testId: string) {
      return screen.getByTestId(testId) as HTMLTextAreaElement;
    },
    select(testId: string) {
      return screen.getByTestId(testId) as HTMLSelectElement;
    },
    checkbox(testId: string) {
      return screen.getByTestId(testId) as HTMLInputElement;
    },
    async ready() {
      await waitFor(() => {
        expect(screen.getByTestId("app-shell")).toBeInTheDocument();
        if (firstPeerId) {
          expect(
            screen.getAllByRole("link", { name: / configuration$/i })[0],
          ).toBeInTheDocument();
        }
      });
    },
    async openChat() {
      const sessionId = bridge.sendResults?.at(-1)?.sessionId;
      if (sessionId) {
        navigate({ name: "session", sessionId });
      } else {
        await user.click(this.chatButton());
      }
      await waitFor(() => {
        expect(screen.getByRole("textbox", { name: "Message" })).toBeInTheDocument();
      });
    },
    async typeComposer(value: string) {
      await user.type(this.composer(), value);
    },
    async clickSend() {
      const button = this.sendButton();
      if (button.hasAttribute("disabled")) {
        throw new Error(`send disabled: ${this.composer().placeholder}`);
      }
      await user.click(button);
    },
    async openConfig() {
      await user.click(this.configButton());
    },
    async openConfigSection(tabId: string) {
      await user.click(this.configSectionTab(tabId));
      await waitFor(() => {
        expect(screen.getByTestId("agent-screen")).toHaveAttribute(
          "data-section",
          tabId,
        );
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
    },
    async openConfigItem(itemId: string) {
      const link = [...document.querySelectorAll<HTMLAnchorElement>("a[href]")].find(
        (candidate) => candidate.getAttribute("href")?.split("/").at(-1) === itemId,
      );
      if (!link) throw new Error(`configuration item ${itemId} is not visible`);
      await user.click(link);
    },
    async replaceInput(testId: string, value: string) {
      fireEvent.change(this.input(testId), { target: { value } });
    },
    async replaceTextarea(testId: string, value: string) {
      fireEvent.change(this.textarea(testId), { target: { value } });
    },
    async selectOption(testId: string, value: string) {
      fireEvent.change(this.select(testId), { target: { value } });
    },
    async setChecked(testId: string, checked: boolean) {
      const checkbox = this.checkbox(testId);
      if (checkbox.checked !== checked) {
        await user.click(checkbox);
      }
    },
    async editBehaviorKey() {
      await user.click(screen.getByTestId("behavior-edit-key"));
    },
    async replaceBehaviorKey(value: string) {
      fireEvent.change(this.behaviorKey(), { target: { value } });
    },
    async replaceBehaviorSystemPrompt(value: string) {
      fireEvent.change(this.behaviorSystemPrompt(), { target: { value } });
    },
    async replaceContextSystemPrompt(value: string) {
      fireEvent.change(this.contextSystemPrompt(), { target: { value } });
    },
    async saveBehaviorConfig() {
      await user.click(this.behaviorSaveButton());
    },
    async pressEnter() {
      const button = this.sendButton();
      if (button.hasAttribute("disabled")) {
        throw new Error(`send disabled: ${this.composer().placeholder}`);
      }
      await user.type(this.composer(), "{enter}");
    },
    async pressShiftEnter() {
      await user.type(this.composer(), "{shift>}{enter}{/shift}");
    },
    cancelButton() {
      return screen.queryByRole("button", { name: "Stop" });
    },
    async clickCancel() {
      const btn = this.cancelButton();
      if (!btn) throw new Error("cancel button not visible");
      await this.user.click(btn);
    },
    cascadeDialog() {
      return screen.queryByRole("dialog", { name: /interrupt parent request/i });
    },
    async confirmCascade() {
      const dialog = this.cascadeDialog();
      if (!dialog) throw new Error("cascade dialog not open");
      const confirm = screen.getByRole("button", {
        name: /interrupt parent and cascade/i,
      });
      await this.user.click(confirm);
    },
    async dispose() {
      try {
        rendered.unmount();
      } finally {
        setDesktopShellTimingConfigForTests(null);
      }
      await bridge.dispose?.();
    },
  };
}
