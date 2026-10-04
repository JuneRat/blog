import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// antd 的浏览器级重置（body 边距、字体等）；组件自身的重置由 <App> 提供。
import "antd/dist/reset.css";
import { App } from "./App";
import { ErrorBoundary } from "./ErrorBoundary";
import { AuthProvider } from "./auth";

const container = document.getElementById("root");
if (container === null) throw new Error("缺少 #root 容器");

createRoot(container).render(
  <StrictMode>
    <ErrorBoundary>
      <AuthProvider>
        <App />
      </AuthProvider>
    </ErrorBoundary>
  </StrictMode>,
);
