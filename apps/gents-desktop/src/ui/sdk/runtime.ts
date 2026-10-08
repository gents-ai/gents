/* Runtime SDK injection, the other half of the vscode-module model. A
   bundled plugin resolves `@gents/ux-sdk` through the Vite alias; a
   runtime-loaded one imports the same specifier and gets the same object:
   the loader rewrites bare specifiers to shim modules that re-export the
   live namespaces installed here. React ships as the app's singletons; a
   second React instance would break hooks. After hermes-agent's
   sdk/runtime.ts. */
import * as React from "react";
import * as jsxRuntime from "react/jsx-runtime";
import * as sdk from "./index";

/* resolved lazily, never at module scope: this module sits in an import
   cycle (sdk/index -> contrib -> loader -> sdk/runtime), so a module-scope
   read of `sdk` would see the hoisted, not-yet-evaluated namespace */
function namespaces() {
  return {
    __GENTS_UX_SDK__: sdk,
    __GENTS_REACT__: React,
    __GENTS_REACT_JSX__: jsxRuntime,
  };
}

type GlobalKey = keyof ReturnType<typeof namespaces>;

export function installUxSdk(): void {
  Object.assign(globalThis, namespaces());
}

/* a shim ESM blob that re-exports a global namespace's live members; the
   export names come from the namespace, so the list cannot drift */
function shimUrl(globalKey: GlobalKey): string {
  const names = Object.keys(namespaces()[globalKey]).filter(
    (name) => name !== "default" && /^[A-Za-z_$][\w$]*$/.test(name),
  );
  const source =
    `const m = globalThis.${globalKey};\n` +
    `export default m.default ?? m;\n` +
    (names.length ? `export const { ${names.join(", ")} } = m;\n` : "");
  return URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
}

let cached: Record<string, string> | null = null;

/** specifier -> shim URL, for the loader */
export function sdkImportMap(): Record<string, string> {
  cached ??= {
    "@gents/ux-sdk": shimUrl("__GENTS_UX_SDK__"),
    "react/jsx-runtime": shimUrl("__GENTS_REACT_JSX__"),
    react: shimUrl("__GENTS_REACT__"),
  };
  return cached;
}

/** the specifiers a runtime plugin may import, in one place */
export const ALLOWED_SPECIFIERS = [
  "@gents/ux-sdk",
  "react",
  "react/jsx-runtime",
] as const;

/** test seam: forget the blob URLs so a fresh install is observable */
export function resetSdkImportMap(): void {
  cached = null;
}
