import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import type { FoundPack } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";

/** One page of the registry's answer, with the search it answers. */
export type RegistryPage = {
  query: string;
  found: FoundPack[];
  page: number;
  hasMore: boolean;
};

/** The pack registry, searched as the person types: the first page shows as
    soon as it mounts, and a new query searches again once typing pauses.
    With `keep`, the page last shown for the same search shows again instead
    of searching anew, and each page shown is kept. */
export function useRegistrySearch(
  initial = "",
  keep?: { last: RegistryPage | null; save: (page: RegistryPage) => void },
) {
  const { actions } = useApp();
  const [kept] = useState(() =>
    keep?.last && keep.last.query === initial.trim() ? keep.last : null,
  );
  const [query, setQuery] = useState(initial);
  const [found, setFound] = useState<FoundPack[]>(kept?.found ?? []);
  const [page, setPage] = useState(kept?.page ?? 1);
  const [hasMore, setHasMore] = useState(kept?.hasMore ?? false);
  /* null until the first answer; a search is in flight while true */
  const [searched, setSearched] = useState<boolean | null>(kept ? false : null);
  const searches = useRef(0);
  const save = useRef(keep?.save);
  save.current = keep?.save;
  /* the search the shown page answers */
  const answered = useRef(kept?.query ?? null);

  /* an empty query lists every pack, a page at a time; an answer to a search
     typed over since is dropped */
  async function search(next: number, q = query) {
    const mine = ++searches.current;
    answered.current = q.trim();
    setSearched((was) => (was === null ? null : true));
    try {
      const report = await actions.searchPacks(q.trim(), next);
      if (mine !== searches.current) return;
      setFound(report.packs);
      setHasMore(report.has_more);
      setPage(next);
      save.current?.({
        query: q.trim(),
        found: report.packs,
        page: next,
        hasMore: report.has_more,
      });
    } catch (error) {
      if (mine === searches.current) toast.error(errorText(error));
    } finally {
      if (mine === searches.current) setSearched(false);
    }
  }

  const typed = useRef(false);
  useEffect(() => {
    if (answered.current === query.trim()) return;
    const wait = typed.current ? 300 : 0;
    typed.current = true;
    const timer = setTimeout(() => void search(1, query), wait);
    return () => clearTimeout(timer);
    /* only a change of query searches again; search reads the rest fresh */
  }, [query]);

  return { query, setQuery, found, page, hasMore, searched, search };
}
