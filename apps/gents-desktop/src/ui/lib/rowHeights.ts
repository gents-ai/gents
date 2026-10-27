/* A transcript row's height before it is drawn. The list places rows it
   has not measured by this guess, so the scroller's length, and with it
   the scroll bar, follow the guesses until the rows are drawn: the closer
   they are, the less either moves. */

export type RowShape =
  | { kind: "user"; text: string; attachments?: number }
  | { kind: "reply"; text: string; changedFiles?: number }
  | { kind: "group"; open: boolean; steps: number }
  | { kind: "other" };

/* text in the transcript's body face: about this many pixels a character,
   this many a line */
const CHAR_PX = 7.6;
const LINE_PX = 24;
const CODE_LINE_PX = 20;

/* the lines a text takes in a column this wide: each paragraph wraps on
   its own, and a fenced block keeps its lines */
export function linesOf(text: string, width: number): { prose: number; code: number } {
  const perLine = Math.max(20, Math.floor(width / CHAR_PX));
  let prose = 0;
  let code = 0;
  let fenced = false;
  for (const line of text.split("\n")) {
    if (line.trimStart().startsWith("```")) {
      fenced = !fenced;
      code += 1;
    } else if (fenced) code += 1;
    else if (line.trim()) prose += Math.ceil(line.length / perLine);
  }
  return { prose: Math.max(1, prose), code };
}

/* the card of files a turn changed, under its final reply: a gap, its
   padding and header, a row a file up to the few it lists, and a link to
   the rest */
export const CARD_FILES = 5;
const changesCardHeight = (files: number) =>
  files === 0
    ? 0
    : 8 + 12 + 24 + 28 * Math.min(files, CARD_FILES) + (files > CARD_FILES ? 24 : 0);

export function guessHeight(shape: RowShape, width: number): number {
  switch (shape.kind) {
    case "user": {
      const { prose, code } = linesOf(shape.text, width - 48);
      return (
        68 +
        LINE_PX * prose +
        CODE_LINE_PX * code +
        (shape.attachments
          ? 96 * Math.ceil(shape.attachments / Math.max(1, Math.floor(width / 248)))
          : 0)
      );
    }
    case "reply": {
      const { prose, code } = linesOf(shape.text, width);
      return (
        52 +
        LINE_PX * prose +
        (code ? 24 + CODE_LINE_PX * code : 0) +
        changesCardHeight(shape.changedFiles ?? 0)
      );
    }
    case "group":
      return shape.open ? 48 + 36 * Math.min(shape.steps, 8) : 48;
    case "other":
      return 60;
  }
}

/* how far the guesses of each kind run from what this session draws:
   learned from the first rows measured, then held, so later rows do not
   keep moving the rows not yet drawn */
const LEARNED_FROM = 24;

export function createHeightLearner() {
  const seen = new Map<string, { n: number; ratio: number }>();
  return {
    learn(kind: RowShape["kind"], guessed: number, measured: number) {
      if (guessed <= 0 || measured <= 0) return;
      const at = seen.get(kind) ?? { n: 0, ratio: 0 };
      if (at.n >= LEARNED_FROM) return;
      at.ratio = (at.ratio * at.n + measured / guessed) / (at.n + 1);
      at.n += 1;
      seen.set(kind, at);
    },
    scale(kind: RowShape["kind"]): number {
      const at = seen.get(kind);
      return at && at.n >= 3 ? Math.min(2, Math.max(0.5, at.ratio)) : 1;
    },
  };
}
