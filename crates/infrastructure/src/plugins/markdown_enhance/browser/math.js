import katex from "katex";
import { roots, failed } from "./common.js";

for (const root of roots()) {
  for (const node of root.querySelectorAll("span.math.math-inline, span.math.math-display")) {
    if (node.dataset.mdEnhanced) continue;
    const source = node.textContent;
    try {
      katex.render(source, node, {
        displayMode: node.classList.contains("math-display"),
        throwOnError: true,
        trust: false,
        maxExpand: 1000,
        maxSize: 20,
        output: "htmlAndMathml",
      });
      node.dataset.mdEnhanced = "done";
    } catch {
      node.textContent = source;
      failed(node, "公式");
    }
  }
}
