/* The inventory of every UX plugin the app knows about, plus the user's
   persisted enable/disable choices. The UX Plugins panel renders this; the
   loaders publish into it and consult the decisions before registering.
   A record carries the loader's own activate/deactivate handles, so a
   toggle never needs an app reload. After hermes-agent's
   contrib/plugins-store.ts, with useSyncExternalStore in place of nanostores
   (the app has no nanostores dependency). */
import { useSyncExternalStore } from "react";

export type UxPluginDoor = "bundled" | "dev" | "pack";
export type UxPluginProducer = "file" | "afb";
export type UxPluginStatus = "disabled" | "error" | "loaded";

export interface UxPluginRecord {
  id: string;
  name: string;
  door: UxPluginDoor;
  status: UxPluginStatus;
  description?: string;
  error?: string;
  /** where the module came from: a file path, or the afb that produced it */
  file?: string;
  producer?: UxPluginProducer;
  /** `namespace/name` of the pack this plugin ships in (pack door) */
  pack?: string;
  /** areas and directive names the manifest declared; the gate the context enforces */
  declared?: UxDeclaredContributions;
}

export interface UxDeclaredContributions {
  areas: readonly string[];
  directives: readonly string[];
}

/* Explicit choices, id -> boolean. Absence means no choice: the plugin's own
   defaultEnabled applies. Absence is not "enabled", which is what lets an
   opt-in plugin ship off. */
const DECISIONS_KEY = "gents.ux.decisions.v1";

function loadDecisions(): Record<string, boolean> {
  try {
    const raw = window.localStorage.getItem(DECISIONS_KEY);
    if (raw) return JSON.parse(raw) as Record<string, boolean>;
  } catch {
    /* no storage, or an unreadable value: no choices */
  }
  return {};
}

interface UxHandle {
  activate: () => Promise<void> | void;
  deactivate: () => void;
}

let decisions: Record<string, boolean> = loadDecisions();
let records: Record<string, UxPluginRecord> = {};
const handles = new Map<string, UxHandle>();
const listeners = new Set<() => void>();

function notify() {
  for (const fn of listeners) fn();
}

function subscribe(fn: () => void) {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

export function uxDecisions(): Readonly<Record<string, boolean>> {
  return decisions;
}

export function uxPluginRecords(): Readonly<Record<string, UxPluginRecord>> {
  return records;
}

export function useUxPluginRecords(): Readonly<Record<string, UxPluginRecord>> {
  return useSyncExternalStore(subscribe, uxPluginRecords, uxPluginRecords);
}

export function useUxDecisions(): Readonly<Record<string, boolean>> {
  return useSyncExternalStore(subscribe, uxDecisions, uxDecisions);
}

/** whether a plugin registers: the explicit choice, else its own default */
export function uxPluginActive(id: string, defaultEnabled = true): boolean {
  return id in decisions ? decisions[id]! : defaultEnabled;
}

function saveDecisions(next: Record<string, boolean>) {
  decisions = next;
  try {
    window.localStorage.setItem(DECISIONS_KEY, JSON.stringify(next));
  } catch {
    /* nonfatal */
  }
  notify();
}

export function publishUxPlugin(record: UxPluginRecord, handle?: UxHandle): void {
  records = { ...records, [record.id]: record };
  if (handle) handles.set(record.id, handle);
  notify();
}

export function patchUxPlugin(id: string, patch: Partial<UxPluginRecord>): void {
  const current = records[id];
  if (!current) return;
  records = { ...records, [id]: { ...current, ...patch } };
  notify();
}

export function dropUxPlugin(id: string): void {
  const { [id]: _dropped, ...rest } = records;
  records = rest;
  handles.delete(id);
  notify();
}

/** live toggle: deactivate and remember, or remember and reactivate */
export async function setUxPluginEnabled(id: string, enabled: boolean): Promise<void> {
  saveDecisions({ ...decisions, [id]: enabled });
  const handle = handles.get(id);
  if (!handle) return;
  if (enabled) await handle.activate();
  else {
    handle.deactivate();
    patchUxPlugin(id, { status: "disabled" });
  }
}

/** test seam: deactivate every live plugin, then forget every record,
    handle and decision */
export function resetUxPluginStore(): void {
  for (const handle of handles.values()) {
    try {
      handle.deactivate();
    } catch {
      /* a test's broken plugin must not block the reset */
    }
  }
  records = {};
  handles.clear();
  decisions = {};
  try {
    window.localStorage.removeItem(DECISIONS_KEY);
  } catch {
    /* nonfatal */
  }
  notify();
}
