"use strict";
const form = document.querySelector("#install-form");
const fields = document.querySelector("#fields");
const adminFields = document.querySelector("#admin-fields");
const submit = document.querySelector("#submit");
const message = document.querySelector("#message");
const token = document.querySelector("#token");
const database = document.querySelector("#database");
const publicUrl = document.querySelector("#public-url");
const password = document.querySelector("#password");
const confirmPassword = document.querySelector("#confirm");
let submitting = false;
let verifiedToken = null;
let verifiedDatabase = null;

function syncForm() {
  const verified = verifiedToken !== null && verifiedToken === token.value;
  const connected = verified && verifiedDatabase !== null && verifiedDatabase === database.value;
  fields.hidden = !verified;
  fields.disabled = submitting || !verified;
  adminFields.hidden = !connected;
  adminFields.disabled = submitting || !connected;
  token.readOnly = submitting;
  submit.disabled = submitting;
  submit.textContent = submitting
    ? (connected ? "正在安装…" : "正在验证…")
    : (!verified ? "验证安装码" : (connected ? "安装博客" : "验证数据库连接"));
}

async function refreshInfo(installToken) {
  const response = await fetch("/api/install", {
    cache: "no-store",
    headers: { "X-Install-Token": installToken },
  });
  if (response.status === 404) {
    location.replace("/admin/");
    return false;
  }
  if (!response.ok) throw new Error(response.status === 403
    ? "安装码无效或请求来源不正确，请核对部署时设置的安装码或启动日志。"
    : "暂时无法读取安装状态，请重试。");
  const info = await response.json();
  if (token.value !== installToken) return false;
  database.required = !info.database_configured;
  document.querySelector("#database-fields").hidden = info.database_configured;
  document.querySelector("#resume").hidden = !info.database_configured;
  if (info.database_configured) database.value = "";
  publicUrl.readOnly = Boolean(info.public_base_url);
  if (info.public_base_url) publicUrl.value = info.public_base_url;
  else if (!publicUrl.value) publicUrl.value = location.origin;
  verifiedToken = installToken;
  verifiedDatabase = null;
  return true;
}

token.addEventListener("input", () => {
  verifiedToken = null;
  verifiedDatabase = null;
  message.textContent = "";
  syncForm();
});
database.addEventListener("input", () => {
  verifiedDatabase = null;
  message.textContent = "";
  syncForm();
});
confirmPassword.addEventListener("input", () => confirmPassword.setCustomValidity(""));
password.addEventListener("input", () => confirmPassword.setCustomValidity(""));
form.addEventListener("submit", async event => {
  event.preventDefault();
  if (submitting) return;
  const installToken = token.value;
  if (!installToken) { token.reportValidity(); return; }
  const authorized = verifiedToken === installToken;
  const installing = authorized && verifiedDatabase !== null && verifiedDatabase === database.value;
  if (installing && password.value !== confirmPassword.value) {
    confirmPassword.setCustomValidity("两次输入的密码不一致");
    confirmPassword.reportValidity();
    return;
  }
  submitting = true;
  syncForm();
  message.textContent = "";
  message.dataset.state = "error";
  try {
    if (!authorized) {
      await refreshInfo(installToken);
      return;
    }
    if (!installing) {
      const databaseUrl = database.value;
      const response = await fetch("/api/install/check", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Install-Token": installToken },
        body: JSON.stringify({ database_url: databaseUrl }),
      });
      if (response.status === 404) { location.replace("/admin/"); return; }
      const result = await response.json();
      if (!response.ok || result.ready !== true) {
        if (response.status === 403) verifiedToken = null;
        throw new Error(result.error || "暂时无法验证数据库连接，请重试。");
      }
      if (token.value === installToken && database.value === databaseUrl) {
        verifiedDatabase = databaseUrl;
        message.dataset.state = "success";
        message.textContent = "数据库连接与安装权限验证通过。请创建管理员并执行安装。";
      }
      return;
    }
    const response = await fetch("/api/install", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Install-Token": installToken },
      body: JSON.stringify({
        database_url: database.value,
        public_base_url: publicUrl.value,
        username: document.querySelector("#username").value,
        password: password.value,
      }),
    });
    if (response.status === 404) { location.replace("/admin/"); return; }
    const result = await response.json();
    if (!response.ok) {
      throw new Error(response.status === 403 ? "安装码无效或请求来源不正确，请核对部署时设置的安装码或启动日志。" : result.error || "安装未完成，请重试。");
    }
    form.reset();
    location.replace(result.redirect);
  } catch (error) {
    message.textContent = error instanceof TypeError ? "连接中断，请重试；已完成的安装不会重复创建账号。" : error.message;
    verifiedDatabase = null;
    if (installing) {
      verifiedToken = null;
      try { await refreshInfo(installToken); } catch { /* 保留原始错误与填写值，重试前重新验证安装码。 */ }
    }
  } finally {
    submitting = false;
    syncForm();
  }
});
