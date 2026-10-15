import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
} from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";

export function useListWindow<T>(
  rows: readonly T[],
  scroller: HTMLElement | null,
  keyOf: (row: T) => string,
  sizeOf: (row: T) => number,
) {
  const ref = useRef<HTMLUListElement>(null);
  const enabled = rows.length > 200;
  const [margin, setMargin] = useState(0);
  const [focused, setFocused] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
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
    enabled,
    initialRect: { width: 0, height: 800 },
    overscan: 8,
    scrollMargin: margin,
    useFlushSync: false,
    rangeExtractor: (range) => {
      const indexes = new Set(defaultRangeExtractor(range));
      for (const key of [focused, pending]) {
        const index = key === null ? undefined : keys.get(key);
        if (index !== undefined) indexes.add(index);
      }
      return [...indexes].sort((a, b) => a - b);
    },
  });
  useLayoutEffect(() => {
    const list = ref.current;
    if (!scroller || !list) return;
    const measure = () =>
      setMargin(
        list.getBoundingClientRect().top -
          scroller.getBoundingClientRect().top +
          scroller.scrollTop,
      );
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(list.parentElement!);
    return () => observer.disconnect();
  }, [scroller]);
  const items = enabled
    ? virtual.getVirtualItems()
    : rows.map((_, index) => ({ index, start: 0 }));
  useLayoutEffect(() => {
    const index = pending === null ? undefined : keys.get(pending);
    if (index === undefined) return;
    const row = ref.current?.querySelector<HTMLElement>(`[data-index="${index}"]`);
    const target = row?.querySelector<HTMLElement>(
      'a[href], button:not([disabled]), [tabindex="0"]',
    );
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
      const row = (e.target as HTMLElement).closest<HTMLLIElement>("li[data-index]");
      if (row && e.currentTarget.contains(row))
        setFocused(getItemKey(Number(row.dataset.index)));
    },
    onBlurCapture: (e) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocused(null);
    },
    onKeyDownCapture: (e) => {
      if (!enabled || e.altKey || e.ctrlKey || e.metaKey) return;
      const target = e.target as HTMLElement;
      const row = target.closest<HTMLLIElement>("li[data-index]");
      if (!row || !e.currentTarget.contains(row)) return;
      const index = Number(row.dataset.index);
      let next: number;
      if (e.key === "Tab") {
        const controls = row.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), [tabindex="0"]',
        );
        if (target !== controls[e.shiftKey ? 0 : controls.length - 1]) return;
        next = index + (e.shiftKey ? -1 : 1);
      } else if (e.key === "ArrowDown") next = index + 1;
      else if (e.key === "ArrowUp") next = index - 1;
      else if (e.key === "Home") next = 0;
      else if (e.key === "End") next = rows.length - 1;
      else return;
      if (next < 0 || next >= rows.length) return;
      e.preventDefault();
      setPending(getItemKey(next));
      virtual.scrollToIndex(next, { align: "auto" });
    },
  };
  return {
    enabled,
    listProps,
    items,
    attributes(index: number, start: number): ComponentProps<"li"> | undefined {
      return enabled
        ? ({
            ref: virtual.measureElement,
            "data-index": index,
            "aria-posinset": index + 1,
            "aria-setsize": rows.length,
            style: {
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${start - margin}px)`,
            },
          } as ComponentProps<"li">)
        : undefined;
    },
  };
}
