import { useState } from "react";
import { createRoot } from "react-dom/client";
import { Dialog, DialogContent, DialogTitle } from "@gents/ui/components/dialog";
import { Sheet, SheetContent, SheetTitle } from "@gents/ui/components/sheet";
import { useMobileVisualViewport } from "../../src/hooks/useMobileVisualViewport";
import { useMobileBackSwipe } from "../../src/hooks/useMobileBackSwipe";
import { EditorSheet } from "../../src/ui/screens/agent/EditorSheet";
import "../../src/App.css";

function MobileHarness() {
  const [backs, setBacks] = useState(0);
  const [editor, setEditor] = useState(false);
  const [dialog, setDialog] = useState(false);
  const [bottom, setBottom] = useState(false);
  useMobileVisualViewport();
  useMobileBackSwipe(true, () => setBacks((n) => n + 1));
  return (
    <div className="viewport-frame overflow-y-auto p-4">
      <output data-testid="back-count">{backs}</output>
      <button onClick={() => setEditor(true)}>Open editor</button>
      <button onClick={() => setDialog(true)}>Open dialog</button>
      <button onClick={() => setBottom(true)}>Open bottom sheet</button>
      <div data-testid="page-swipe" className="h-20">
        Page
      </div>
      <div data-testid="horizontal" className="w-60 overflow-x-auto">
        <div className="w-[900px]">Wide content</div>
      </div>
      <textarea aria-label="Draft" />
      <div
        contentEditable
        suppressContentEditableWarning
        role="textbox"
        aria-label="Rich draft"
      />
      <EditorSheet open={editor} onClose={() => setEditor(false)} title="Mobile editor">
        <input aria-label="First field" />
        <div className="h-[450px]" />
        <input aria-label="Last field" />
      </EditorSheet>
      <Dialog open={dialog} onOpenChange={setDialog}>
        <DialogContent>
          <DialogTitle>Mobile dialog</DialogTitle>
          <input aria-label="Dialog field" />
          <div className="h-[450px] shrink-0">Long dialog content</div>
          <button>Save dialog</button>
        </DialogContent>
      </Dialog>
      <Sheet open={bottom} onOpenChange={setBottom}>
        <SheetContent side="bottom">
          <SheetTitle>Bottom sheet</SheetTitle>
          <input aria-label="Bottom field" />
        </SheetContent>
      </Sheet>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<MobileHarness />);
