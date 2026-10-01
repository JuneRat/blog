import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { vditorAssets } from "./build/vditorAssets.ts";

/**
 * 开发期同源策略（docs/identity-and-admin.md「可信身份与入口」）：
 * Vite 代理 /api 与 /auth 到后端，使浏览器看到的后端与 SPA 同源。
 *
 * 不要设置 `changeOrigin`（默认 false）。设为 true 会把 Host 改写成
 * 127.0.0.1:8080，而浏览器发出的 Origin 仍是 http://localhost:5173，
 * 后端的同源校验会拒绝写请求（403）——正确做法是代理保持 Host 不变，
 * 而不是给后端加开发期白名单。
 *
 * 配套不变量（见 docs/development.md「后台 SPA 联调」）：
 * 1. 浏览器与后端统一使用同一个主机名，不要混用 localhost 与 127.0.0.1；
 * 2. 开发期后端设 BLOG_PUBLIC_BASE_URL=http://localhost:5173，
 *    并在 OIDC/GitHub 侧为 dev 客户端注册 /auth/callback/{id} 回调。
 */
export default defineConfig({
  // 生产挂在 /admin 子树下（Rust 侧 mount_admin_spa）。
  base: "/admin/",
  plugins: [react(), vditorAssets()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    // Keep the scheduling polyfill out of the bootstrap's shared runtime chunk.
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [{ name: "temporal", test: /node_modules[\\/](?:@js-temporal[\\/]polyfill|jsbi)[\\/]/ }],
        },
      },
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:3000" },
      "/auth": { target: "http://127.0.0.1:3000" },
      "/assets/plugins": { target: "http://127.0.0.1:3000" },
      "/media": { target: "http://127.0.0.1:3000" },
    },
  },
});
