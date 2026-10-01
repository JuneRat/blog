import mermaid from "mermaid";
import { roots, failed } from "./common.js";

// No automatic document-wide scan; only explicitly marked Markdown code blocks.
mermaid.initialize({
  startOnLoad: false,
  securityLevel: "strict",
  suppressErrorRendering: true,
  htmlLabels: false,
  fontFamily: "system-ui, sans-serif",
  maxTextSize: 50000,
  maxEdges: 500,
  // Diagram frontmatter/directives must not loosen these host settings.
  secure: ["secure", "securityLevel", "startOnLoad", "suppressErrorRendering", "htmlLabels",
    "maxTextSize", "maxEdges", "fontFamily", "dompurifyConfig", "themeCSS"],
});

async function renderDiagrams() {
  let index = 0;
  for (const root of roots()) {
    for (const code of root.querySelectorAll("pre > code.language-mermaid")) {
      const sourceNode = code.parentElement;
      if (sourceNode.dataset.mdEnhanced) continue;
      sourceNode.dataset.mdEnhanced = "pending";
      const source = code.textContent;
      // A separate container keeps source text intact if parsing/layout fails.
      const output = document.createElement("div");
      output.className = "md-enhance-diagram";
      sourceNode.after(output);
      let id;
      do { id = `md-enhance-diagram-${++index}`; } while (document.getElementById(id));
      try {
        const { svg } = await mermaid.render(id, source, output);
        output.innerHTML = svg; // Mermaid's strict renderer sanitizes generated SVG.
        const drawing = output.querySelector("svg");
        const width = drawing?.viewBox.baseVal.width;
        if (width > 0 && Number.isFinite(width)) {
          // Keep labels readable; scroll wide diagrams on small screens.
          drawing.style.width = `${Math.ceil(width)}px`;
          drawing.style.maxWidth = "none";
        }
        output.dataset.mdEnhanced = "done";
        sourceNode.remove();
      } catch {
        output.remove();
        failed(sourceNode, "图表");
      }
    }
  }
}

void renderDiagrams();
