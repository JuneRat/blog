/* Plain text only: never insert comment content with innerHTML. */
(() => {
  const root = document.querySelector('[data-comments-slug]');
  if (!root) return;
  // getRandomValues also works on HTTP previews where randomUUID is unavailable.
  function requestId() {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    bytes[6] = (bytes[6] & 15) | 64; bytes[8] = (bytes[8] & 63) | 128;
    const hex = Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
    return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
  }
  const endpoint = `/api/v1/posts/${encodeURIComponent(root.dataset.commentsSlug)}/comments`;
  const node = (tag, text) => { const n = document.createElement(tag); if (text) n.textContent = text; return n; };
  const notice = node('p'); notice.setAttribute('role', 'status');
  const list = node('div');
  const formArea = node('div');
  root.append(node('h2', '评论'), notice, list, formArea);
  let enabled = false;
  let me;
  // Keep live form nodes (including drafts and idempotency keys) across pagination.
  const forms = new Map();
  async function request(url, options) {
    const r = await fetch(url, { credentials: 'same-origin', cache: 'no-store', ...options });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) { const error = new Error(data.error || '暂时无法加载，请重试'); error.status = r.status; throw error; }
    return data;
  }
  function clearUnavailable(e) {
    if (e.status === 404) { list.replaceChildren(); formArea.replaceChildren(); forms.clear(); enabled = false; }
    notice.textContent = e.message;
  }
  async function refreshIdentity() {
    let current;
    try { current = await request('/api/admin/v1/me'); }
    catch (e) { if (e.status !== 401) throw e; }
    me = current;
    for (const entry of forms.values()) entry.syncIdentity();
  }
  function form(parentId, container) {
    const existing = forms.get(parentId);
    if (existing) { container.append(existing.element); return; }
    const f = node('form');
    const nickname = node('input'); nickname.name = 'nickname'; nickname.maxLength = 64; nickname.required = true; nickname.autocomplete = 'nickname';
    const nameLabel = node('label');
    const nameCaption = node('span'); nameLabel.append(nameCaption, nickname);
    const body = node('textarea'); body.name = 'body'; body.maxLength = 2000; body.required = true; body.rows = 5;
    const bodyLabel = node('label', parentId ? '回复（最多 2,000 字）' : '评论（最多 2,000 字）'); bodyLabel.append(body);
    const send = node('button', '提交审核'); send.type = 'submit';
    const message = node('p'); message.setAttribute('role', 'status');
    f.append(nameLabel, bodyLabel, send, message);
    let key = requestId();
    let previous;
    let submitting = false;
    function syncIdentity() {
      nameCaption.textContent = me ? '已登录身份' : '昵称';
      if (me) nickname.value = me.display_name || me.username || '作者';
      nickname.disabled = submitting || !!me;
    }
    syncIdentity();
    forms.set(parentId, { element: f, syncIdentity });
    f.addEventListener('submit', async event => {
      event.preventDefault();
      if (submitting) return;
      submitting = true; send.disabled = true; body.disabled = true; nickname.disabled = true;
      const payload = { nickname: nickname.value || '作者', body: body.value, parent_id: parentId };
      const signature = JSON.stringify(payload);
      if (previous && previous !== signature) key = requestId();
      previous = signature;
      try {
        const result = await request(endpoint, { method: 'POST', headers: { 'Content-Type': 'application/json', ...(me ? { 'X-CSRF-Token': me.csrf_token } : {}) }, body: JSON.stringify({ ...payload, request_id: key }) });
        message.textContent = result.message;
        body.value = ''; key = requestId(); previous = undefined;
      } catch (e) {
        message.textContent = e.message;
        if (e.status === 404) clearUnavailable(e);
        if (e.status === 401) {
          // /me clears only a confirmed invalid cookie. Keep the draft and let
          // the reader confirm the new identity before explicitly resubmitting.
          try {
            await refreshIdentity();
            message.textContent = me ? '登录状态已更新，请确认身份后再次提交。' : '登录已失效，请确认昵称后再次提交。';
          } catch { message.textContent = '身份校验失败，请稍后重试；评论正文已保留。'; }
        }
      }
      finally { submitting = false; send.disabled = false; body.disabled = false; syncIdentity(); }
    });
    container.append(f);
  }
  async function load(container, parent = null, page = 1) {
    const params = new URLSearchParams({ page });
    if (parent) params.set('parent_id', parent);
    const data = await request(`${endpoint}?${params}`);
    enabled = data.enabled;
    container.replaceChildren();
    if (!parent) {
      notice.textContent = `${data.total} 条主评论${enabled ? ' · 审核后展示' : ' · 新评论已关闭'}`;
      formArea.replaceChildren(); if (enabled) form(null, formArea);
    }
    for (const item of data.items) {
      const article = node('article'); article.className = 'comment-item';
      const identity = node('div'); identity.className = 'comment-identity';
      identity.append(node('strong', item.nickname));
      if (item.is_author) {
        const badge = node('span', '作者'); badge.className = 'comment-author-badge';
        badge.setAttribute('aria-label', '文章作者'); identity.append(badge);
      }
      article.append(identity, node('time', item.created_at));
      const text = node('p', item.body); text.className = 'comment-body'; article.append(text);
      if (!parent) {
        const replies = node('div'); replies.className = 'comment-replies';
        const show = node('button', '查看回复'); show.type = 'button';
        show.onclick = async () => { show.disabled = true; try { await load(replies, item.id); } catch (e) { clearUnavailable(e); } finally { show.disabled = false; } };
        article.append(show);
        if (enabled) {
          const reply = node('button', '回复'); reply.type = 'button';
          const replyForm = node('div');
          reply.onclick = () => { replyForm.replaceChildren(); form(item.id, replyForm); };
          if (forms.has(item.id)) form(item.id, replyForm);
          article.append(reply, replyForm);
        }
        article.append(replies);
      }
      container.append(article);
    }
    if (!data.items.length) container.append(node('p', parent ? '暂无已通过的回复。' : '暂无评论，欢迎留下想法。'));
    const nav = node('div'); nav.className = 'comment-pagination';
    for (const [label, target, allowed] of [['上一页', page-1, page>1], ['下一页', page+1, page*20<data.total]]) {
      const button = node('button', label); button.type = 'button'; button.disabled = !allowed;
      button.onclick = async () => { button.disabled = true; try { await load(container, parent, target); } catch (e) { clearUnavailable(e); button.disabled = false; } };
      nav.append(button);
    }
    nav.append(node('span', `第 ${page} 页`)); container.append(nav);
  }
  (async () => {
    try { await refreshIdentity(); } catch { notice.textContent = '身份校验失败，请刷新后重试。'; return; }
    try { await load(list); } catch (e) { clearUnavailable(e); }
  })();
})();
