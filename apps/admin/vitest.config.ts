import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

/**
 * 前端单元测试配置（与 vite.config.ts 的构建配置分开）。
 *
 * 只覆盖可在 jsdom 中驱动的编辑器交互与路由解析；
 * 真实登录、SSR 与页面可见性仍由 Rust 侧的集成测试保证。
 *
 * `include` 必须同时覆盖 `src/` 与 `tests/`：只留一个会让另一处的用例静默不被
 * 收集，CI 照样全绿（`tests/editor.test.tsx` 就曾因此被吞掉）。
 */
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}", "tests/**/*.test.{ts,tsx}"],
  },
});
