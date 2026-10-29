import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyChosenTheme } from "./ui/preferences";
import { sendAppOpened } from "./lib/nativeShell";

applyChosenTheme();
sendAppOpened();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
