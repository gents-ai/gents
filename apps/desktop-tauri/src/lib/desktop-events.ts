import { listen } from "@tauri-apps/api/event";

export type DesktopClientUpdatedHandler = () => void | Promise<void>;
export type DesktopClientUpdatedUnlisten = () => void;
export type DesktopClientUpdatedListenerFactory = (
  handler: DesktopClientUpdatedHandler,
) => Promise<DesktopClientUpdatedUnlisten>;

async function defaultDesktopClientUpdatedListenerFactory(
  handler: DesktopClientUpdatedHandler,
) {
  const unlisten = await listen("desktop://client-updated", () => {
    void handler();
  });
  return () => {
    unlisten();
  };
}

let desktopClientUpdatedListenerFactoryOverride:
  | DesktopClientUpdatedListenerFactory
  | null = null;

export function setDesktopClientUpdatedListenerFactoryForTests(
  factory: DesktopClientUpdatedListenerFactory | null,
) {
  desktopClientUpdatedListenerFactoryOverride = factory;
}

export function listenToDesktopClientUpdates(
  handler: DesktopClientUpdatedHandler,
) {
  return (
    desktopClientUpdatedListenerFactoryOverride ??
    defaultDesktopClientUpdatedListenerFactory
  )(handler);
}
