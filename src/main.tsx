import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyTheme, loadStoredTheme, useThemeStore } from "./store/themeStore";
import "./styles/globals.css";

// 首帧前应用持久化主题，避免浅色用户启动时闪暗色
applyTheme(loadStoredTheme());
useThemeStore.setState({ theme: loadStoredTheme() });

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
