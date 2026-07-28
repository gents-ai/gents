import { listen } from "@tauri-apps/api/event";
import type { ClientUpdateEvent } from "./transport.js";

export type DesktopClientUpdatedEvent = ClientUpdateEvent;
export type DesktopClientUpdatedHandler = (
  event: DesktopClientUpdatedEvent,
) => void | Promise<void>;
export type DesktopClientUpdatedUnlisten = () => void;
export type DesktopClientUpdatedListenerFactory = (
  handler: DesktopClientUpdatedHandler,
) => Promise<DesktopClientUpdatedUnlisten>;

async function defaultDesktopClientUpdatedListenerFactory(
  handler: DesktopClientUpdatedHandler,
) {
  const unlisten = await listen<DesktopClientUpdatedEvent>(
    "desktop://client-updated",
    (event) => {
      void handler(event.payload ?? {});
    },
  );
  return () => {
    unlisten();
  };
}

let desktopClientUpdatedListenerFactoryOverride: DesktopClientUpdatedListenerFactory | null =
  null;

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
