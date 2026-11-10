import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import type { FoundPack } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";

/** The pack registry, searched as the person types: the first page shows as
    soon as it mounts, and a new query searches again once typing pauses. */
export function useRegistrySearch(initial = "") {
  const { actions } = useApp();
  const [query, setQuery] = useState(initial);
  const [found, setFound] = useState<FoundPack[]>([]);
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  /* null until the first answer; a search is in flight while true */
  const [searched, setSearched] = useState<boolean | null>(null);
  const searches = useRef(0);

  /* an empty query lists every pack, a page at a time; an answer to a search
     typed over since is dropped */
  async function search(next: number, q = query) {
    const mine = ++searches.current;
    setSearched((was) => (was === null ? null : true));
    try {
      const report = await actions.searchPacks(q.trim(), next);
      if (mine !== searches.current) return;
      setFound(report.packs);
      setHasMore(report.has_more);
      setPage(next);
    } catch (error) {
      if (mine === searches.current) toast.error(errorText(error));
    } finally {
      if (mine === searches.current) setSearched(false);
    }
  }

  const typed = useRef(false);
  useEffect(() => {
    const wait = typed.current ? 300 : 0;
    typed.current = true;
    const timer = setTimeout(() => void search(1, query), wait);
    return () => clearTimeout(timer);
    /* only a change of query searches again; search reads the rest fresh */
  }, [query]);

  return { query, setQuery, found, page, hasMore, searched, search };
}
