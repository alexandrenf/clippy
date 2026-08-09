import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyTheme } from "./store";
import "./styles.css";

// Match macOS appearance before React mounts to avoid a bright first frame.
applyTheme("system");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
