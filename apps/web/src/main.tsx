import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

// Apply the saved theme before React mounts so a refresh never flashes the
// opposite palette while the workspace snapshot is loading.
const initialTheme = window.localStorage.getItem("aialra-theme") === "dark" ? "dark" : "light";
document.documentElement.dataset.theme = initialTheme;
document.documentElement.style.colorScheme = initialTheme;

// StrictMode surfaces unsafe effects before the same UI is packaged in Tauri.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

if (import.meta.env.PROD && "serviceWorker" in navigator) {
  window.addEventListener("load", () => void navigator.serviceWorker.register("/service-worker.js"));
}
