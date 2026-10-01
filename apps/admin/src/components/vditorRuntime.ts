import type Vditor from "vditor";

declare const __VDITOR_CDN__: string;
export const editorCdn = __VDITOR_CDN__;
const scripts = new Map<string, Promise<void>>();
function loadScript(path: string, id: string) {
  if (!scripts.has(id)) scripts.set(id, new Promise<void>((resolve, reject) => {
    if (document.getElementById(id)) { resolve(); return; }
    const script = document.createElement("script");
    script.src = `${editorCdn}/${path}`;
    script.onload = () => { script.id = id; resolve(); };
    script.onerror = () => { script.remove(); scripts.delete(id); reject(new Error("编辑器资源加载失败，可重试或继续编辑 Markdown 源码。")); };
    document.head.appendChild(script);
  }));
  return scripts.get(id)!;
}
export async function loadVditor(): Promise<typeof Vditor> {
  const [module] = await Promise.all([
    import("vditor"),
    loadScript("dist/js/i18n/zh_CN.js", "vditorI18nScriptzh_CN"),
    loadScript("dist/js/lute/lute.min.js", "vditorLuteScript"),
  ]);
  return module.default;
}
