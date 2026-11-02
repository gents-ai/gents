import { createContext, useCallback, useContext, useId, useMemo, useRef } from "react";
import { flushSync } from "react-dom";

type PartRef = (element: HTMLElement | null) => (() => void) | undefined;

type Morph = {
  /** applies `update`, animating this owner's parts from before to after */
  morph: (update: () => void) => void;
  /** a ref marking an element as part `key` of the card; elements under one
      key, shown one at a time, morph into each other */
  part: (key: string) => PartRef;
};

const still: Morph = { morph: (update) => update(), part: () => () => undefined };

const MorphContext = createContext<Morph>(still);

/** The nearest card owner's morph, for content inside a card that changes it.
    Outside any owner, changes simply apply. */
export const useMorph = () => useContext(MorphContext);

export const MorphProvider = MorphContext.Provider;

/* the newest morph to name an element, so a transition that a quicker one
   skipped leaves that one's names alone when it ends */
const namedBy = new WeakMap<HTMLElement, object>();

/** Owns a card that changes as a view transition: its parts (`part`) animate
    from their old size to their new one while their contents cross-fade
    (morph.css), and nothing else is captured. A view transition captures every
    element with a `view-transition-name`, so the parts take theirs, unique to
    this owner, only while its own morph runs: a card in another context, or
    another instance of this one, holds still. Without the API, or with reduced
    motion, an update simply applies. */
export function useMorphCard(): Morph {
  const scope = `morph-${useId().replace(/[^\w-]/g, "")}`;
  const parts = useRef(new Map<string, Set<HTMLElement>>());
  const refs = useRef(new Map<string, PartRef>());

  const part = useCallback((key: string) => {
    let ref = refs.current.get(key);
    if (!ref) {
      ref = (element) => {
        if (!element) return undefined;
        const shown = parts.current.get(key) ?? new Set<HTMLElement>();
        parts.current.set(key, shown.add(element));
        return () => void shown.delete(element);
      };
      refs.current.set(key, ref);
    }
    return ref;
  }, []);

  const morph = useCallback(
    (update: () => void) => {
      const doc = document as Document & {
        startViewTransition?: (callback: () => void) => { finished?: Promise<unknown> };
      };
      if (
        !doc.startViewTransition ||
        matchMedia("(prefers-reduced-motion: reduce)").matches
      )
        return update();
      const token = {};
      const each = (run: (element: HTMLElement, name: string) => void) => {
        for (const [key, elements] of parts.current)
          for (const element of elements) run(element, `${scope}-${key}`);
      };
      const name = () =>
        each((element, name) => {
          element.style.setProperty("view-transition-name", name);
          namedBy.set(element, token);
        });
      name();
      /* an update can mount new elements for a part; they take its name
         before the new state is captured */
      const transition = doc.startViewTransition(() => {
        flushSync(update);
        name();
      });
      void Promise.resolve(transition?.finished).finally(() =>
        each((element) => {
          if (namedBy.get(element) !== token) return;
          element.style.removeProperty("view-transition-name");
          namedBy.delete(element);
        }),
      );
    },
    [scope],
  );

  return useMemo(() => ({ morph, part }), [morph, part]);
}
