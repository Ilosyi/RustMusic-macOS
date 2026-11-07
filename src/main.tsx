import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
import { initTheme } from "./theme";
import { applySkin, loadSkin } from "./skins";

initTheme();
applySkin(loadSkin());
// 平台标记（CSS 按平台差异化）。UA 即时可得，无异步闪烁；
// Windows WebView2 恒含 "Windows NT"，macOS WKWebView 恒含 "Macintosh"
const ua = navigator.userAgent;
document.documentElement.dataset.platform = /windows/i.test(ua)
  ? "windows"
  : /macintosh|mac os x/i.test(ua)
    ? "macos"
    : "linux";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
