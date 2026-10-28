import { act, fireEvent } from "@testing-library/react";
import { $createParagraphNode, $getRoot, type LexicalEditor } from "lexical";

/* The composer's field is a rich editor (the kit's RichComposer). jsdom
   can't type into it the way a browser does, so tests set its text through
   the editor, which is what typing ends in, and read the text it sends. */
type Editable = HTMLElement & { __lexicalEditor?: LexicalEditor };

const editorOf = (box: HTMLElement) => (box as Editable).__lexicalEditor;

export function typeInComposer(box: HTMLElement, value: string) {
  const editor = editorOf(box);
  if (!editor) {
    fireEvent.change(box, { target: { value } });
    return;
  }
  act(() => {
    editor.update(
      () => {
        const root = $getRoot();
        root.clear();
        const paragraph = $createParagraphNode();
        root.append(paragraph);
        const selection = paragraph.select();
        if (value) selection.insertRawText(value);
      },
      { discrete: true },
    );
  });
}

export function composerText(box: HTMLElement) {
  const editor = editorOf(box);
  if (!editor) return (box as HTMLTextAreaElement).value;
  return editor.getEditorState().read(() => $getRoot().getTextContent());
}

/** user-event's `type` on the composer: appends the text, then Enter when
    it ends in {Enter}, and leaves the field focused for later keys */
export async function typeText(box: HTMLElement, input: string) {
  const enter = input.endsWith("{Enter}");
  const text = enter ? input.slice(0, -"{Enter}".length) : input;
  typeInComposer(box, composerText(box) + text);
  act(() => box.focus());
  if (enter) await act(async () => void fireEvent.keyDown(box, { key: "Enter" }));
}
