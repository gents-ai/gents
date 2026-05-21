import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect } from "vitest";

import App from "../src/App";
import { setDesktopShellTimingConfigForTests } from "../src/hooks/useDesktopShell";
import {
  setDesktopApiAdapterForTests,
  type DesktopApiAdapter,
} from "../src/lib/desktop-api";
import {
  setDesktopClientUpdatedListenerFactoryForTests,
  type DesktopClientUpdatedListenerFactory,
} from "../src/lib/desktop-events";

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
  setDesktopApiAdapterForTests(bridge.adapter);
  setDesktopClientUpdatedListenerFactoryForTests(bridge.listenerFactory);
  setDesktopShellTimingConfigForTests(timingConfig);

  const user = userEvent.setup();
  const rendered = render(<App />);

  return {
    bridge,
    user,
    composer() {
      return screen.getByTestId("composer-input") as HTMLTextAreaElement;
    },
    sendButton() {
      return screen.getByTestId("composer-send");
    },
    conversation(sessionId: string) {
      return screen.getByTestId(`conversation-${sessionId}`);
    },
    configButton() {
      if (firstPeerId) {
        const fleetConfig = screen.queryByTestId(`fleet-config-${firstPeerId}`);
        if (fleetConfig) {
          return fleetConfig;
        }
        return screen.getByTestId(`deployment-config-${firstPeerId}`);
      }
      return screen.getAllByLabelText(/^Configure /)[0];
    },
    chatButton() {
      if (firstPeerId) {
        return screen.getByTestId(`fleet-chat-${firstPeerId}`);
      }
      return screen.getAllByLabelText(/^Open .* chat/)[0];
    },
    configSectionTab(tabId: string) {
      return screen.getByTestId(`config-tab-${tabId}`);
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
        expect(screen.getByTestId("fleet-dashboard")).toBeInTheDocument();
        if (firstPeerId) {
          expect(screen.getByTestId(`fleet-row-${firstPeerId}`)).toBeInTheDocument();
        }
      });
    },
    async openChat() {
      await user.click(this.chatButton());
      await waitFor(() => {
        expect(screen.getByTestId("composer-input")).toBeInTheDocument();
      });
    },
    async typeComposer(value: string) {
      await user.type(this.composer(), value);
    },
    async clickSend() {
      await user.click(this.sendButton());
    },
    async openConfig() {
      await user.click(this.configButton());
    },
    async openConfigSection(tabId: string) {
      await user.click(this.configSectionTab(tabId));
      await waitFor(() => {
        expect(this.configSectionTab(tabId)).toHaveClass("selected");
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
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
    async saveBehaviorConfig() {
      await user.click(this.behaviorSaveButton());
    },
    async pressEnter() {
      await user.type(this.composer(), "{enter}");
    },
    async pressShiftEnter() {
      await user.type(this.composer(), "{shift>}{enter}{/shift}");
    },
    cancelButton() {
      return screen.queryByTestId("cancel-button");
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
      const confirm = screen.getByRole("button", { name: /interrupt parent and cascade/i });
      await this.user.click(confirm);
    },
    async dispose() {
      try {
        rendered.unmount();
      } finally {
        setDesktopApiAdapterForTests(null);
        setDesktopClientUpdatedListenerFactoryForTests(null);
        setDesktopShellTimingConfigForTests(null);
      }
      await bridge.dispose?.();
    },
  };
}
