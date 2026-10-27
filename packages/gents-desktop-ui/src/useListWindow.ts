import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
} from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";

export const LIST_WINDOW_THRESHOLD = 200;
const ROW_CONTROLS = 'a[href], button:not([disabled]), summary, [tabindex="0"]';

export function useListWindow<T>(
  rows: readonly T[],
  scroller: HTMLElement | null,
  keyOf: (row: T) => string,
  sizeOf: (row: T) => number,
  options: {
    enabled?: boolean;
    anchorTo?: "end";
    stickyKey?: string | null;
  } = {},
) {
  const ref = useRef<HTMLUListElement>(null);
  const active = options.enabled !== false;
  const enabled = active && rows.length > LIST_WINDOW_THRESHOLD;
  const [margin, setMargin] = useState(0);
  const [focused, setFocused] = useState<string | null>(null);
  const [pending, setPending] = useState<{ key: string; last: boolean } | null>(
    null,
  );
  const keys = useMemo(
    () => new Map(rows.map((row, i) => [keyOf(row), i])),
    [rows, keyOf],
  );
  const getItemKey = useCallback((i: number) => keyOf(rows[i]!), [rows, keyOf]);
  const virtual = useVirtualizer<HTMLElement, HTMLLIElement>({
    count: rows.length,
    getScrollElement: () => scroller,
    estimateSize: (i) => sizeOf(rows[i]!),
    getItemKey,
    enabled: active,
    anchorTo: enabled ? options.anchorTo : undefined,
    initialRect: { width: 0, height: 800 },
    overscan: 8,
    scrollMargin: margin,
    useFlushSync: false,
    rangeExtractor: (range) => {
      const indexes = new Set(defaultRangeExtractor(range));
      for (const key of [
        focused,
        pending?.key ?? null,
        options.stickyKey ?? null,
      ]) {
        const index = key === null ? undefined : keys.get(key);
        if (index !== undefined) indexes.add(index);
      }
      return [...indexes].sort((a, b) => a - b);
    },
  });
  // Keep row measurements across the threshold; non-windowed scroll correction
  // belongs to the transcript's prepend owner.
  virtual.shouldAdjustScrollPositionOnItemSizeChange = enabled
    ? undefined
    : () => false;
  useLayoutEffect(() => {
    const list = ref.current;
    if (!active || !scroller || !list) return;
    const measure = () =>
      setMargin(
        list.getBoundingClientRect().top -
          scroller.getBoundingClientRect().top +
          scroller.scrollTop,
      );
    measure();
    if (typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(measure);
    observer.observe(list.parentElement!);
    return () => observer.disconnect();
  }, [active, scroller]);
  const stickyIndex = options.stickyKey
    ? keys.get(options.stickyKey)
    : undefined;
  const stickySize = virtual
    .getVirtualItems()
    .find((item) => item.index === stickyIndex)?.size;
  const canStick =
    stickySize !== undefined &&
    !!scroller &&
    stickySize <= scroller.clientHeight / 3;
  const items = enabled
    ? virtual.getVirtualItems()
    : rows.map((_, index) => ({ index, start: 0 }));
  useLayoutEffect(() => {
    const index = pending === null ? undefined : keys.get(pending.key);
    if (index === undefined) return;
    const row = ref.current?.querySelector<HTMLElement>(
      `[data-index="${index}"]`,
    );
    const controls = row?.querySelectorAll<HTMLElement>(ROW_CONTROLS);
    const target = controls?.[pending?.last ? controls.length - 1 : 0] ?? row;
    if (target) {
      target.focus({ preventScroll: true });
      setPending(null);
    }
  }, [pending, keys, items]);
  const listProps: ComponentProps<"ul"> = {
    ref,
    style: enabled
      ? { position: "relative", height: virtual.getTotalSize() }
      : undefined,
    onFocusCapture: (e) => {
      const row = (e.target as HTMLElement).closest<HTMLLIElement>(
        "li[data-index]",
      );
      if (row && e.currentTarget.contains(row))
        setFocused(getItemKey(Number(row.dataset.index)));
    },
    onBlurCapture: (e) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node | null))
        setFocused(null);
    },
    onKeyDownCapture: (e) => {
      if (!enabled || e.altKey || e.ctrlKey || e.metaKey) return;
      const target = e.target as HTMLElement;
      const row = target.closest<HTMLLIElement>("li[data-index]");
      if (!row || !e.currentTarget.contains(row)) return;
      const index = Number(row.dataset.index);
      let next: number;
      if (e.key === "Tab") {
        const controls = row.querySelectorAll<HTMLElement>(ROW_CONTROLS);
        if (
          target !== row &&
          target !== controls[e.shiftKey ? 0 : controls.length - 1]
        )
          return;
        next = index + (e.shiftKey ? -1 : 1);
      } else if (e.key === "ArrowDown") next = index + 1;
      else if (e.key === "ArrowUp") next = index - 1;
      else if (e.key === "Home") next = 0;
      else if (e.key === "End") next = rows.length - 1;
      else return;
      if (next < 0 || next >= rows.length) return;
      e.preventDefault();
      setPending({
        key: getItemKey(next),
        last: e.key === "Tab" && e.shiftKey,
      });
      virtual.scrollToIndex(next, { align: "auto" });
    },
  };
  return {
    enabled,
    listProps,
    items,
    attributes(index: number, start: number): ComponentProps<"li"> {
      return {
        ref: active ? virtual.measureElement : undefined,
        "data-index": index,
        "aria-posinset": index + 1,
        "aria-setsize": rows.length,
        tabIndex: -1,
        style: enabled
          ? {
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${
                canStick && index === stickyIndex
                  ? Math.max(
                      start - margin,
                      (virtual.scrollOffset ?? 0) - margin,
                    )
                  : start - margin
              }px)`,
              ...(canStick && index === stickyIndex ? { zIndex: 1 } : {}),
            }
          : canStick && index === stickyIndex
            ? { position: "sticky", top: 0, zIndex: 1 }
            : undefined,
      } as ComponentProps<"li">;
    },
  };
}
