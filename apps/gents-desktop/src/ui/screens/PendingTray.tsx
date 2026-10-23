import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent,
} from "react";
import { CornerDownRight, Ellipsis, GripVertical, Pencil, Trash2 } from "lucide-react";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { useApp, useView } from "../app/AppContext";
import { useDragReorder } from "../hooks/useDragReorder";
import { useSelectedSessionFields } from "../hooks/useSelectedSession";
import {
  moveEdit,
  moveRun,
  pendingRows,
  removeEdit,
  separatedBySettings,
  steerEdit,
  steerRun,
  type PendingRow,
  type QueueEdit,
  type QueueSteer,
} from "@source-inc/gents-desktop-client";
import { Hint } from "./Hint";
import { useComposerEdit } from "./session/composerEdit";
import { AutomatedInput } from "./TranscriptInputs";

export function useSessionQueue() {
  const { actions } = useApp();
  const pending = useSelectedSessionFields((session) =>
    session
      ? {
          sessionId: session.sessionId,
          nodeDid: session.nodeDid,
          queuedTurns: session.queuedTurns,
          pendingQueue: session.pendingQueue,
        }
      : null,
  );
  const sessionId = pending?.sessionId;
  const nodeDid = pending?.nodeDid;
  const onEdit =
    sessionId && nodeDid
      ? async (edit: QueueEdit) => {
          await actions.editPendingQueue({ ...edit, nodeDid, sessionId });
        }
      : undefined;
  const onSteer =
    sessionId && nodeDid
      ? async (steer: QueueSteer) => {
          await actions.steerPendingMessage({ ...steer, nodeDid, sessionId });
        }
      : undefined;
  return { pending, queue: pending?.pendingQueue ?? null, onEdit, onSteer };
}

/** The selected session's tray, reading its own queue, with a message just
    sent behind a running turn shown from the moment it is sent, until the
    queue lists it. */
export function SelectedPendingTray() {
  const { pending, queue, onEdit, onSteer } = useSessionQueue();
  const edit = useComposerEdit();
  const local = useView((view) => view.pendingTurn);
  const localQueued = useView(
    (view) =>
      view.sentTurns.find((sent) => sent.requestId === local?.requestId)?.queued ??
      false,
  );
  const listed = pending?.queuedTurns ?? NO_TURNS;
  const queued = useMemo(
    () =>
      local &&
      localQueued &&
      local.sessionId === pending?.sessionId &&
      !listed.some((turn) => turn.requestId === local.requestId)
        ? [...listed, local]
        : listed,
    [local, localQueued, pending?.sessionId, listed],
  );
  if (!pending?.sessionId) return null;
  return (
    <PendingTray
      key={pending.sessionId}
      queued={queued}
      queue={queue}
      onEdit={onEdit}
      onSteer={onSteer}
      editingDocId={edit?.editingDocId ?? null}
      onBeginEdit={edit?.begin}
    />
  );
}

export function PendingTray({
  queued,
  queue = null,
  onEdit,
  onSteer,
  editingDocId = null,
  onBeginEdit,
}: {
  queued: readonly PendingTurnView[];
  queue?: readonly PendingQueueEntryView[] | null;
  onEdit?: (edit: QueueEdit) => Promise<void>;
  onSteer?: (steer: QueueSteer) => Promise<void>;
  editingDocId?: string | null;
  onBeginEdit?: (entry: PendingQueueEntryView) => void;
}) {
  const rows = useMemo(() => pendingRows(queued, queue), [queued, queue]);
  const [busy, setBusy] = useState(false);
  const edited = useRef(new Set<string>()).current;
  const locked = busy || editingDocId !== null;
  const apply = async (edit: QueueEdit | null): Promise<boolean> => {
    if (!edit || !onEdit || !queue || locked) return false;
    for (const docId of edit.selectedRequestDocIds) {
      const requestId = queue.find((entry) => entry.requestDocId === docId)?.requestId;
      if (requestId) edited.add(requestId);
    }
    setBusy(true);
    try {
      await onEdit(edit);
      return true;
    } catch {
      // The action owner reports the failure; the runtime retains the queue.
      return false;
    } finally {
      setBusy(false);
    }
  };
  /* the messages steered leave the queue for the running turn, so they show
     as picked up rather than as an edit */
  const steer = async (entry: PendingQueueEntryView) => {
    if (!onSteer || !queue || locked) return;
    setBusy(true);
    try {
      await onSteer(steerEdit(queue, entry));
    } catch {
      // The action owner reports the failure; the runtime retains the queue.
    } finally {
      setBusy(false);
    }
  };
  const editable = Boolean(onEdit && queue);
  const canMove = (from: number, to: number) => {
    const a = rows[from];
    const b = rows[to];
    return Boolean(
      editable &&
      !locked &&
      a?.kind === "message" &&
      a.entry &&
      b?.kind === "message" &&
      b.entry &&
      moveRun(queue!, a.index, b.index),
    );
  };
  const move = (from: number, to: number) => {
    const a = rows[from];
    const b = rows[to];
    if (a?.kind !== "message" || b?.kind !== "message" || !queue)
      return Promise.resolve(false);
    return apply(moveEdit(queue, a.index, b.index));
  };
  const separated = (from: number, to: number) => {
    const a = rows[from];
    const b = rows[to];
    return Boolean(
      queue &&
      a?.kind === "message" &&
      b?.kind === "message" &&
      separatedBySettings(queue, a.index, b.index),
    );
  };
  const taken = useTaken(queue, edited);
  const leaving = useLeavingRows(rows, edited);
  const reorder = useDragReorder<HTMLDivElement>({
    axis: "y",
    keys: rows.map((row) => row.turn.requestId),
    canMove,
    commit: move,
    animateReorders: true,
  });
  if (rows.length === 0 && leaving.length === 0) return null;

  return (
    <div
      ref={reorder.container}
      className="mx-3 -mb-px overflow-hidden rounded-t-2xl border border-b-0 border-border bg-raised"
      data-testid="queued-inputs"
      role="list"
      aria-label="Pending messages"
    >
      {leaving.map((row) => (
        <div
          key={`leaving:${row.turn.requestId}`}
          aria-hidden
          data-testid="leaving-input"
          className="grid animate-[tray-row-leave_200ms_ease-out_forwards] motion-reduce:hidden"
        >
          <div className="min-h-0 overflow-hidden">
            <div className="border-b border-border/60 bg-raised px-3 py-1.5 text-sm">
              <TakenRow row={row} />
            </div>
          </div>
        </div>
      ))}
      {rows.map((row, position) => {
        const entry = row.kind === "message" && editable ? row.entry : null;
        const draggable = Boolean(
          entry && (canMove(position, position - 1) || canMove(position, position + 1)),
        );
        const beingEdited = Boolean(entry && entry.requestDocId === editingDocId);
        return (
          <div
            key={row.turn.requestId}
            role="listitem"
            data-testid="queued-input"
            data-request-id={row.turn.requestId}
            data-reorder-key={row.turn.requestId}
            data-editing={beingEdited || undefined}
            style={reorder.style(position)}
            {...(draggable ? rowDrag(reorder.handle(position)) : {})}
            className={cn(
              "border-b border-border/60 bg-raised px-3 py-1.5 text-sm select-none last:border-b-0",
              draggable && "cursor-grab active:cursor-grabbing",
              reorder.moving === position && "relative z-10",
              reorder.lifted === position && "shadow-md ring-1 ring-border",
              beingEdited && "shadow-[inset_2px_0_0_var(--color-brand)]",
            )}
          >
            {taken(row.turn.requestId) ? (
              <TakenRow row={row} />
            ) : row.kind === "automated" ? (
              row.turn.origin && (
                <AutomatedInput
                  origin={row.turn.origin}
                  content={row.turn.content}
                  state="Queued"
                />
              )
            ) : (
              <MessageRow
                row={row}
                behindQueued={Boolean(
                  row.entry &&
                  queue?.slice(0, row.index).some((before) => before.steerable),
                )}
                own={Boolean(entry)}
                draggable={draggable}
                editing={beingEdited}
                actions={
                  entry && {
                    locked,
                    onEdit: onBeginEdit ? () => onBeginEdit(entry) : null,
                    onRemove: () => void apply(removeEdit(queue!, entry)),
                    steer: onSteer ? steerRun(queue!, row.index) : null,
                    onSteer: () => void steer(entry),
                    up: canMove(position, position - 1)
                      ? () => void move(position, position - 1)
                      : null,
                    down: canMove(position, position + 1)
                      ? () => void move(position, position + 1)
                      : null,
                    separated:
                      separated(position, position - 1) ||
                      separated(position, position + 1),
                  }
                }
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

const NO_TURNS: readonly PendingTurnView[] = [];

function useTaken(
  queue: readonly PendingQueueEntryView[] | null,
  edited: ReadonlySet<string>,
): (requestId: string) => boolean {
  const seen = useRef(new Set<string>()).current;
  for (const entry of queue ?? []) seen.add(entry.requestId);
  const listed = new Set(queue?.map((entry) => entry.requestId));
  return (requestId) =>
    queue !== null &&
    seen.has(requestId) &&
    !listed.has(requestId) &&
    !edited.has(requestId);
}

const LEAVE_MS = 200;

function useLeavingRows(rows: PendingRow[], edited: ReadonlySet<string>): PendingRow[] {
  const previous = useRef(rows);
  const timers = useRef(new Set<ReturnType<typeof setTimeout>>());
  const [leaving, setLeaving] = useState<PendingRow[]>([]);
  useLayoutEffect(() => {
    const before = previous.current;
    previous.current = rows;
    const had = new Set(before.map((row) => row.turn.requestId));
    if (rows.some((row) => !had.has(row.turn.requestId))) return;
    const now = new Set(rows.map((row) => row.turn.requestId));
    const gone = before.filter(
      (row) => !now.has(row.turn.requestId) && !edited.has(row.turn.requestId),
    );
    if (gone.length === 0) return;
    setLeaving((current) => [...current, ...gone]);
    const timer = setTimeout(() => {
      timers.current.delete(timer);
      setLeaving((current) => current.filter((row) => !gone.includes(row)));
    }, LEAVE_MS);
    timers.current.add(timer);
  }, [rows, edited]);
  useEffect(() => {
    const pending = timers.current;
    return () => pending.forEach(clearTimeout);
  }, []);
  return leaving.filter(
    (row) => !rows.some((current) => current.turn.requestId === row.turn.requestId),
  );
}

function TakenRow({ row }: { row: PendingRow }) {
  return (
    <div className="flex min-w-0 items-center gap-2" data-taken>
      <CornerDownRight
        aria-hidden
        className="size-3.5 shrink-0 text-muted-foreground"
      />
      <span className="min-w-0 flex-1 truncate text-muted-foreground">
        {row.turn.content}
      </span>
      <span className="shrink-0 text-xs text-muted-foreground">Picked up</span>
    </div>
  );
}

function rowDrag(handle: ReturnType<ReturnType<typeof useDragReorder>["handle"]>) {
  return {
    ...handle,
    onPointerDown: (event: PointerEvent<HTMLElement>) => {
      const target = event.target as HTMLElement;
      if (!event.currentTarget.contains(target)) return;
      if (target.closest("button, a, input, textarea")) return;
      handle.onPointerDown(event);
    },
  };
}

const SLOW_DIM = "disabled:delay-300";

function MessageRow({
  row,
  behindQueued,
  own,
  draggable,
  editing,
  actions,
}: {
  row: Extract<PendingRow, { kind: "message" }>;
  /** the runtime delivers in order, so a steer waits behind a queued message */
  behindQueued: boolean;
  own: boolean;
  draggable: boolean;
  editing: boolean;
  actions: {
    locked: boolean;
    onEdit: (() => void) | null;
    onRemove: () => void;
    /** the messages a steer sends, this one last; null when it can't */
    steer: readonly PendingQueueEntryView[] | null;
    onSteer: () => void;
    up: (() => void) | null;
    down: (() => void) | null;
    separated: boolean;
  } | null;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      {own ? (
        <span
          aria-hidden
          data-testid="pending-grip"
          data-draggable={draggable || undefined}
          className={cn(
            "-my-1 -ml-1 grid h-6 w-4 shrink-0 touch-none place-items-center text-muted-foreground",
            !draggable && "opacity-40",
          )}
        >
          <GripVertical className="size-3.5" />
        </span>
      ) : (
        <CornerDownRight
          aria-hidden
          className="size-3.5 shrink-0 text-muted-foreground"
        />
      )}
      <span
        className={cn("min-w-0 flex-1 truncate", editing && "text-muted-foreground")}
        title={
          row.entry?.steerable
            ? "Sends after the running turn finishes"
            : behindQueued
              ? "Waits for the queued message before it"
              : "Steers in at the next step"
        }
      >
        {row.turn.content}
      </span>
      {editing ? (
        <span className="shrink-0 text-xs text-muted-foreground">Editing…</span>
      ) : (
        actions && (
          <span className="flex shrink-0 items-center">
            {actions.steer && (
              <Hint label={steerLabel(actions.steer.length)}>
                <Button
                  size="xs"
                  variant="quiet"
                  className={cn("px-2", SLOW_DIM)}
                  disabled={actions.locked}
                  onClick={actions.onSteer}
                >
                  Steer
                </Button>
              </Hint>
            )}
            {actions.onEdit && (
              <Button
                size="icon-xs"
                variant="quiet"
                aria-label="Edit"
                className={SLOW_DIM}
                disabled={actions.locked}
                onClick={actions.onEdit}
              >
                <Pencil />
              </Button>
            )}
            <Button
              size="icon-xs"
              variant="quiet"
              aria-label="Remove"
              className={SLOW_DIM}
              disabled={actions.locked}
              onClick={actions.onRemove}
            >
              <Trash2 />
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button
                    size="icon-xs"
                    variant="quiet"
                    aria-label="More for this message"
                    className={SLOW_DIM}
                    disabled={actions.locked}
                  />
                }
              >
                <Ellipsis />
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" side="top" className="w-44">
                <DropdownMenuGroup>
                  <DropdownMenuItem
                    aria-label="Move pending message up"
                    disabled={!actions.up}
                    onClick={actions.up ?? undefined}
                  >
                    Move up
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    aria-label="Move pending message down"
                    disabled={!actions.down}
                    onClick={actions.down ?? undefined}
                  >
                    Move down
                  </DropdownMenuItem>
                </DropdownMenuGroup>
                {actions.separated && (
                  <p className="px-2 py-1.5 text-xs text-muted-foreground">
                    Messages sent with different settings can’t be reordered together.
                  </p>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          </span>
        )
      )}
    </div>
  );
}

const steerLabel = (sent: number) =>
  "Send into the running turn at the next safe point" +
  (sent > 2
    ? `, with the ${sent - 1} messages before it`
    : sent === 2
      ? ", with the message before it"
      : "");
