import React from "react";
import ReactDOM from "react-dom/client";
import { bindVisualViewport } from "@gents/shell";
import App from "./App";
import { initTheme } from "./ui/theme";

initTheme();
bindVisualViewport();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
