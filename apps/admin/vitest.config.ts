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
 *
 * `setupFiles` 补 jsdom 缺失的 matchMedia / ResizeObserver：antd 组件在渲染期
 * 就会用到它们，缺了会让所有用例失败在与业务无关的地方。
 */
export default defineConfig({
  plugins: [react()],
  // Public comments and installation tests use the exact Rust-served browser bundles.
  server: {
    fs: { allow: [".", "../../crates/interfaces/assets", "../../crates/interfaces/src/install"] },
  },
  test: {
    // 多屏渲染较重；限制 CPU 争用，并给慢速 CI 留出余量，不用重试掩盖失败。
    maxWorkers: process.env.CI ? 2 : 4,
    testTimeout: 30_000,
    environment: "jsdom",
    setupFiles: ["src/testSetup.ts"],
    include: ["src/**/*.test.{ts,tsx}", "tests/**/*.test.{ts,tsx}"],
  },
});
