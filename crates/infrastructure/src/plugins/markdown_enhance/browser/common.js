export function roots() {
  return document.querySelectorAll("[data-content-root]");
}

export function failed(node, kind) {
  node.dataset.mdEnhanced = "error";
  const message = document.createElement("span");
  message.className = "md-enhance-error";
  message.setAttribute("role", "status");
  message.textContent = `${kind}渲染失败，请检查语法。`;
  node.after(message);
}
