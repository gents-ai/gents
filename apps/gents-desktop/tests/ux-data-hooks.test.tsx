/* `useQuery` against a stand-in for the bounded query tool: pages of at
   most `limit` rows with `next_offset` on a full one, and strings past the
   cap answered as `field_recovery` plus hash-chained `field_page`s. The
   hook must gather whole result sets and whole values without the page
   writing either loop, and must leave a value alone when it changes
   between its pages. */
import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { UxContext, UxDataCallResult } from "@/sandbox/contract";
import { useQuery } from "@/sandbox/data-hooks";

const ROW_CAP = 3;
const STRING_CAP = 8;

interface Row {
  id: string;
  title: string;
}

function tool(rows: Row[], options: { changeAfterPage?: boolean } = {}) {
  const calls: Record<string, unknown>[] = [];
  const hash = (text: string) => `h:${text.length}:${text.slice(0, 2)}`;
  const call = vi.fn(
    async (args: Record<string, unknown>): Promise<UxDataCallResult> => {
      calls.push(args);
      const page = args.field_page as
        | {
            doc_id: string;
            field: string;
            offset_bytes: number;
            expected_hash?: string;
          }
        | undefined;
      if (page) {
        const row = rows.find((r) => r.id === page.doc_id);
        if (!row) throw new Error("field_page document is unavailable");
        let value = row.title;
        if (options.changeAfterPage && page.offset_bytes > 0) value = `${value}!`;
        if (page.offset_bytes > 0 && page.expected_hash !== hash(value)) {
          throw new Error("field_page value changed; restart at offset 0");
        }
        const end = Math.min(value.length, page.offset_bytes + STRING_CAP);
        return {
          kind: "query",
          output: {
            collection: "T",
            field_page: {
              doc_id: page.doc_id,
              field: page.field,
              text: value.slice(page.offset_bytes, end),
              offset_bytes: page.offset_bytes,
              next_offset_bytes: end,
              total_bytes: value.length,
              value_hash: hash(value),
              complete: end === value.length,
            },
          },
        };
      }
      const offset = (args.offset as number | undefined) ?? 0;
      const limit = Math.min((args.limit as number | undefined) ?? ROW_CAP, ROW_CAP);
      const slice = rows.slice(offset, offset + limit);
      const results = slice.map((r) => ({
        title:
          r.title.length > STRING_CAP ? `${r.title.slice(0, STRING_CAP)}…` : r.title,
      }));
      const field_recovery = slice.flatMap((r, row) =>
        r.title.length > STRING_CAP
          ? [
              {
                row,
                doc_id: r.id,
                field: "title",
                total_bytes: r.title.length,
                field_page: { doc_id: r.id, field: "title", offset_bytes: 0 },
              },
            ]
          : [],
      );
      const output: Record<string, unknown> = {
        collection: "T",
        results,
        count: results.length,
      };
      if (field_recovery.length > 0) output.field_recovery = field_recovery;
      if (results.length === limit) {
        output.next_offset = offset + results.length;
        output.limit_note = `Result set hit the ${limit}-row cap`;
      }
      return { kind: "query", output };
    },
  );
  const ctx = {
    data: {
      call: (_s: string, _t: string, args: Record<string, unknown>) => call(args),
    },
    onEvent: () => () => {},
  } as unknown as UxContext;
  return { ctx, call, calls };
}

const rows = (n: number, long: number[] = []): Row[] =>
  Array.from({ length: n }, (_, i) => ({
    id: `d${i}`,
    title: long.includes(i) ? `long value number ${i} goes on and on` : `t${i}`,
  }));

type Out = {
  results: { title: string }[];
  count: number;
  next_offset?: number;
  limit_note?: string;
  field_recovery?: { row: number; doc_id: string }[];
};

describe("useQuery pages", () => {
  it("answers one page by default, next_offset and all", async () => {
    const t = tool(rows(7));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", { intervalMs: 0, live: false }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results.map((r) => r.title)).toEqual([
      "t0",
      "t1",
      "t2",
    ]);
    expect(result.current.data?.next_offset).toBe(3);
    expect(t.call).toHaveBeenCalledTimes(1);
  });

  it("follows next_offset to the end with pages: 'all', count summed, nothing left to page", async () => {
    const t = tool(rows(7));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", { intervalMs: 0, live: false, pages: "all" }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results.map((r) => r.title)).toEqual([
      "t0",
      "t1",
      "t2",
      "t3",
      "t4",
      "t5",
      "t6",
    ]);
    expect(result.current.data?.count).toBe(7);
    expect(result.current.data?.next_offset).toBeUndefined();
    expect(result.current.data?.limit_note).toBeUndefined();
    expect(t.calls.map((c) => c.offset ?? 0)).toEqual([0, 3, 6]);
  });

  it("stops at maxRows and says where the rest starts", async () => {
    const t = tool(rows(20));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", {
        intervalMs: 0,
        live: false,
        pages: "all",
        maxRows: 5,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results).toHaveLength(5);
    expect(result.current.data?.count).toBe(5);
    expect(result.current.data?.next_offset).toBe(5);
    expect(result.current.data?.limit_note).toContain("offset 5");
    expect(t.call).toHaveBeenCalledTimes(2);
  });

  it("passes the caller's own offset and order through untouched", async () => {
    const t = tool(rows(7));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", {
        intervalMs: 0,
        live: false,
        args: { offset: 3, order: { title: "DESC" }, limit: 2 },
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(t.calls[0]).toEqual({ offset: 3, order: { title: "DESC" }, limit: 2 });
    expect(result.current.data?.results.map((r) => r.title)).toEqual(["t3", "t4"]);
    expect(result.current.data?.next_offset).toBe(5);
  });
});

describe("useQuery field recovery", () => {
  it("splices a cut string back together from its hash-chained pages", async () => {
    const t = tool(rows(3, [1]));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", {
        intervalMs: 0,
        live: false,
        args: { fields: ["title"] },
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results[1]?.title).toBe(
      "long value number 1 goes on and on",
    );
    expect(result.current.data?.results[0]?.title).toBe("t0");
    expect(result.current.data?.field_recovery).toBeUndefined();
    const pages = t.calls.filter((c) => c.field_page) as {
      field_page: { offset_bytes: number; expected_hash?: string };
      fields: string[];
    }[];
    expect(pages.map((p) => p.field_page.offset_bytes)).toEqual([0, 8, 16, 24, 32]);
    expect(pages[0].field_page.expected_hash).toBeUndefined();
    expect(
      pages.slice(1).every((p) => typeof p.field_page.expected_hash === "string"),
    ).toBe(true);
    /* the recovered field stays selected on each follow-up */
    expect(pages.every((p) => p.fields.includes("title"))).toBe(true);
  });

  it("leaves a value that changed between pages as answered, recovery entry kept", async () => {
    const t = tool(rows(2, [0]), { changeAfterPage: true });
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", { intervalMs: 0, live: false }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results[0]?.title).toBe("long val…");
    expect(result.current.data?.field_recovery).toEqual([
      expect.objectContaining({ row: 0, doc_id: "d0" }),
    ]);
    expect(result.current.error).toBeNull();
  });

  it("recovers across pages with pages: 'all', keeping each row index in the whole", async () => {
    const t = tool(rows(7, [1, 4]));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", { intervalMs: 0, live: false, pages: "all" }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results[1]?.title).toBe(
      "long value number 1 goes on and on",
    );
    expect(result.current.data?.results[4]?.title).toBe(
      "long value number 4 goes on and on",
    );
    expect(result.current.data?.results[5]?.title).toBe("t5");
    expect(result.current.data?.field_recovery).toBeUndefined();
  });

  it("can be told not to follow", async () => {
    const t = tool(rows(2, [0]));
    const { result } = renderHook(() =>
      useQuery<Out>(t.ctx, "s", "list", {
        intervalMs: 0,
        live: false,
        recoverFields: false,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.results[0]?.title).toBe("long val…");
    expect(result.current.data?.field_recovery).toHaveLength(1);
    expect(t.call).toHaveBeenCalledTimes(1);
  });

  it("drops an answer whose inputs changed while its pages were in flight", async () => {
    const t = tool(rows(7));
    const { result, rerender } = renderHook(
      ({ lane }: { lane: string }) =>
        useQuery<Out>(t.ctx, "s", "list", {
          intervalMs: 0,
          live: false,
          pages: "all",
          args: { lane },
        }),
      { initialProps: { lane: "a" } },
    );
    await act(async () => {
      rerender({ lane: "b" });
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data?.count).toBe(7);
    const lastFirstPage = [...t.calls].reverse().find((c) => !c.offset);
    expect(lastFirstPage?.lane).toBe("b");
  });
});
