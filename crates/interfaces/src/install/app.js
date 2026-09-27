"use strict";
const form = document.querySelector("#install-form");
const fields = document.querySelector("#fields");
const submit = document.querySelector("#submit");
const message = document.querySelector("#message");
const database = document.querySelector("#database");
const publicUrl = document.querySelector("#public-url");
const password = document.querySelector("#password");
const confirmPassword = document.querySelector("#confirm");
let submitting = false;

async function refreshInfo() {
  const response = await fetch("/api/install", { cache: "no-store" });
  if (response.status === 404) {
    location.replace("/admin/");
    return false;
  }
  if (!response.ok) throw new Error("暂时无法读取安装状态，请刷新重试。");
  const info = await response.json();
  database.required = !info.database_configured;
  document.querySelector("#database-fields").hidden = info.database_configured;
  document.querySelector("#resume").hidden = !info.database_configured;
  if (info.database_configured) database.value = "";
  publicUrl.readOnly = Boolean(info.public_base_url);
  if (info.public_base_url) publicUrl.value = info.public_base_url;
  else if (!publicUrl.value) publicUrl.value = location.origin;
  return true;
}

refreshInfo().then(ready => {
  if (ready) { fields.disabled = false; submit.disabled = false; }
}).catch(error => { message.textContent = error.message; });

confirmPassword.addEventListener("input", () => confirmPassword.setCustomValidity(""));
password.addEventListener("input", () => confirmPassword.setCustomValidity(""));
form.addEventListener("submit", async event => {
  event.preventDefault();
  if (submitting) return;
  if (password.value !== confirmPassword.value) {
    confirmPassword.setCustomValidity("两次输入的密码不一致");
    confirmPassword.reportValidity();
    return;
  }
  const input = {
    database_url: database.value,
    public_base_url: publicUrl.value,
    username: document.querySelector("#username").value,
    password: password.value,
  };
  submitting = true;
  fields.disabled = true;
  submit.disabled = true;
  submit.textContent = "正在安装…";
  message.textContent = "";
  try {
    const response = await fetch("/api/install", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Install-Token": document.querySelector("#token").value },
      body: JSON.stringify(input),
    });
    if (response.status === 404) { location.replace("/admin/"); return; }
    const result = await response.json();
    if (!response.ok) {
      throw new Error(response.status === 403 ? "安装码无效或请求来源不正确，请核对终端中的安装码。" : result.error || "安装未完成，请重试。");
    }
    form.reset();
    submit.textContent = "安装完成，正在进入登录页…";
    location.replace(result.redirect);
  } catch (error) {
    message.textContent = error instanceof TypeError ? "连接中断，请重试；已完成的安装不会重复创建账号。" : error.message;
    try { await refreshInfo(); } catch { /* Keep the original error and entered values. */ }
    fields.disabled = false;
    submit.disabled = false;
    submit.textContent = "安装博客";
    submitting = false;
  }
});
