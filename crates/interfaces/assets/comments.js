/* Only server-sanitized content_html enters HTML sinks; names and errors are text. */
(() => {
  const root = document.querySelector('[data-comments-slug]');
  if (!root) return;
  root.hidden = true;
  const endpoint = `/api/v1/posts/${encodeURIComponent(root.dataset.commentsSlug)}/comments`;
  const node = (tag, text) => { const n = document.createElement(tag); if (text) n.textContent = text; return n; };
  const notice = node('p'); notice.setAttribute('role', 'status');
  const refreshComments = node('button', '刷新评论'); refreshComments.type = 'button';
  const list = node('div');
  const formArea = node('div');
  root.append(node('h2', '评论'), notice, refreshComments, list, formArea);
  let enabled = false;
  let guestEnabled = false;
  let me;
  let identityReady = false;
  const identityNotice = node('p');
  identityNotice.setAttribute('role', 'status');
  const retryIdentity = node('button', '刷新登录状态'); retryIdentity.type = 'button'; retryIdentity.hidden = true;
  root.insertBefore(identityNotice, list); root.insertBefore(retryIdentity, list);
  // A readable thread can still have a hidden root that cannot be replied to directly.
  const unavailableThreads = new Set();
  const unavailableComments = new Set();
  // Keep live form nodes (including drafts and guest contact details) across pagination.
  const forms = new Map();
  const views = new Map();
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
  function threadUnavailable(e, parent, container) {
    if (e.status === 404) {
      unavailableThreads.add(parent);
      unavailableComments.add(parent);
      if (container) container.replaceChildren(node('p', '该评论已不可用，回复草稿已保留。'));
      for (const entry of forms.values()) entry.syncIdentity();
    }
    notice.textContent = e.message;
  }
  function threadAvailable(parent) {
    if (!unavailableThreads.delete(parent)) return false;
    for (const [target, entry] of forms) {
      if (target === parent || entry.threadId === parent) entry.restoreAvailability();
    }
    return true;
  }
  function commentUnavailable(e, parent) {
    unavailableComments.add(parent);
    forms.get(parent)?.syncIdentity();
    notice.textContent = e.message;
  }
  function commentAvailable(parent) {
    if (!unavailableComments.delete(parent)) return false;
    forms.get(parent)?.restoreAvailability();
    return true;
  }
  async function refreshIdentity() {
    identityReady = false;
    retryIdentity.disabled = true;
    for (const entry of forms.values()) entry.syncIdentity();
    let current;
    try { current = await request('/api/admin/v1/me'); }
    catch (e) {
      if (e.status !== 401) {
        identityNotice.textContent = '身份校验失败，暂时无法提交；评论正文已保留。';
        retryIdentity.textContent = '重试身份校验';
        retryIdentity.hidden = false; retryIdentity.disabled = false;
        throw e;
      }
    }
    me = current; identityReady = true;
    identityNotice.textContent = ''; retryIdentity.textContent = '刷新登录状态'; retryIdentity.hidden = false; retryIdentity.disabled = false;
    for (const entry of forms.values()) entry.syncIdentity();
  }
  retryIdentity.onclick = () => {
    refreshIdentity().then(() => { identityNotice.textContent = '登录状态已更新，请确认身份后再次提交。'; }).catch(() => {});
  };
  function form(parentId, container, threadId = parentId) {
    const existing = forms.get(parentId);
    if (existing) { container.append(existing.element); return; }
    const f = node('form');
    const loginNotice = node('p', '请登录后发表评论。');
    const loginLink = node('a', '登录');
    loginLink.href = `/admin/?next=${encodeURIComponent(location.pathname + location.search)}`;
    loginNotice.append(loginLink);
    const fields = node('fieldset');
    f.append(loginNotice, fields);
    const nickname = node('input'); nickname.name = 'nickname'; nickname.maxLength = 64; nickname.required = true; nickname.autocomplete = 'nickname';
    const nameLabel = node('label');
    const nameCaption = node('span'); nameLabel.append(nameCaption, nickname);
    const email = node('input'); email.name = 'email'; email.type = 'email'; email.maxLength = 320; email.autocomplete = 'email';
    const emailLabel = node('label', '邮箱（可选，仅管理员可见）'); emailLabel.append(email);
    const body = node('textarea'); body.name = 'body'; body.maxLength = 2000; body.required = true; body.rows = 5;
    const bodyLabel = node('label', parentId ? '回复（最多 2,000 字）' : '评论（最多 2,000 字）'); bodyLabel.append(body);
    const send = node('button', '提交评论'); send.type = 'submit';
    const message = node('p'); message.setAttribute('role', 'status');
    const toolbar = node('div'); toolbar.className = 'comment-toolbar';
    const preview = node('div'); preview.className = 'comment-body comment-preview'; preview.hidden = true;
    const previewButton = node('button', '预览'); previewButton.type = 'button';
    let revision = 0;
    body.addEventListener('input', () => { revision++; preview.hidden = true; });
    for (const [label, before, after, fallback] of [
      ['粗体', '**', '**', '文字'], ['斜体', '*', '*', '文字'], ['代码', '`', '`', '代码'],
      ['链接', '[', '](https://example.com)', '链接文字'], ['引用', '\n> ', '', '引用'], ['列表', '\n- ', '', '项目'],
    ]) {
      const button = node('button', label); button.type = 'button';
      button.onclick = () => {
        if (submitting) return;
        const start = body.selectionStart, end = body.selectionEnd;
        const selected = body.value.slice(start, end) || fallback;
        if (body.value.length - (end - start) + before.length + selected.length + after.length > 2000) return;
        body.setRangeText(before + selected + after, start, end, 'end');
        body.focus(); body.dispatchEvent(new Event('input'));
      };
      toolbar.append(button);
    }
    previewButton.onclick = async () => {
      const current = ++revision;
      previewButton.disabled = true;
      try {
        const result = await request('/api/v1/comments/preview', {method: 'POST', headers: {'Content-Type': 'application/json', ...(me ? {'X-CSRF-Token': me.csrf_token} : {})}, body: JSON.stringify({body: body.value})});
        if (current === revision) { preview.innerHTML = result.content_html; preview.hidden = false; }
      } catch (e) { if (current === revision) message.textContent = e.message; }
      finally { previewButton.disabled = false; }
    };
    toolbar.append(previewButton);
    fields.append(nameLabel, emailLabel, bodyLabel, toolbar, preview, send, message);
    let submitting = false;
    function syncIdentity() {
      const loginRequired = identityReady && !me && !guestEnabled;
      loginNotice.hidden = !loginRequired;
      fields.hidden = loginRequired;
      fields.disabled = loginRequired;
      nameCaption.textContent = me ? '已登录身份' : '昵称';
      if (me) nickname.value = me.display_name || me.username || '作者';
      nickname.disabled = submitting || !!me || !identityReady;
      emailLabel.hidden = !!me; email.disabled = submitting || !!me || !identityReady;
      send.disabled = submitting || !identityReady || !enabled || (!me && !guestEnabled) || unavailableComments.has(parentId) || unavailableThreads.has(threadId);
      if (unavailableComments.has(parentId) || unavailableThreads.has(threadId)) message.textContent = '该评论已不可用，回复草稿已保留。';
    }
    syncIdentity();
    forms.set(parentId, {
      element: f,
      threadId,
      syncIdentity,
      restoreAvailability() { message.textContent = ''; syncIdentity(); },
    });
    f.addEventListener('submit', async event => {
      event.preventDefault();
      if (submitting || !identityReady || !enabled || (!me && !guestEnabled) || unavailableComments.has(parentId) || unavailableThreads.has(threadId)) return;
      submitting = true; revision++; preview.hidden = true; body.disabled = true; syncIdentity();
      const payload = { nickname: nickname.value || '作者', email: me ? null : email.value.trim() || null, body: body.value, parent_id: parentId };
      try {
        const result = await request(endpoint, { method: 'POST', headers: { 'Content-Type': 'application/json', ...(me ? { 'X-CSRF-Token': me.csrf_token } : {}) }, body: JSON.stringify(payload) });
        message.textContent = result.message;
        body.value = ''; revision++;
        if (result.status === 'approved') {
          const view = views.get(parentId ? threadId : null);
          try { await load(view?.container || list, view ? (parentId ? threadId : null) : null, view?.page || 1); }
          catch { message.textContent = '评论已发布，但列表刷新失败，请刷新查看。'; }
        }
      } catch (e) {
        message.textContent = e.status ? e.message : `${e.message}；提交结果未确认，重试可能产生重复评论。`;
        if (e.status === 404) {
          if (parentId) commentUnavailable(e, parentId);
          else notice.textContent = e.message;
        }
        if (e.status === 401) {
          // /me clears only a confirmed invalid cookie. Keep the draft and let
          // the reader confirm the new identity before explicitly resubmitting.
          try {
            await refreshIdentity();
            message.textContent = me ? '登录状态已更新，请确认身份后再次提交。' : (guestEnabled ? '登录已失效，请确认昵称后再次提交。' : '登录已失效，请重新登录后提交。');
          } catch { message.textContent = '身份校验失败，请稍后重试；评论正文已保留。'; }
        }
      }
      finally { submitting = false; send.disabled = false; body.disabled = false; syncIdentity(); }
    });
    container.append(f);
  }
  async function load(container, parent = null, page = 1) {
    const params = new URLSearchParams({ page });
    if (parent) params.set('root_id', parent);
    const data = await request(`${endpoint}?${params}`);
    enabled = data.enabled;
    guestEnabled = data.guest_comments_enabled === true;
    for (const entry of forms.values()) entry.syncIdentity();
    root.hidden = !enabled;
    if (!enabled) {
      list.replaceChildren(); formArea.replaceChildren(); views.clear();
      return;
    }
    if (parent && threadAvailable(parent)) notice.textContent = '该评论已恢复。';
    container.replaceChildren();
    if (!parent) {
      views.clear();
      notice.textContent = `${data.total} 条主评论`;
      formArea.replaceChildren(); form(null, formArea);
    }
    views.set(parent, { container, page });
    for (const item of data.items) {
      if (!item.placeholder && commentAvailable(item.id)) notice.textContent = '该评论已恢复。';
      const article = node('article'); article.className = 'comment-item';
      const identity = node('div'); identity.className = 'comment-identity';
      if (!item.placeholder) identity.append(node('strong', item.nickname));
      if (item.parent_id) identity.append(node('span', `回复 ${item.parent_nickname || '该评论'}`));
      if (item.is_author && !item.placeholder) {
        const badge = node('span', '作者'); badge.className = 'comment-author-badge';
        badge.setAttribute('aria-label', '文章作者'); identity.append(badge);
      }
      const date = new Date(item.created_at);
      const time = node('time', Number.isNaN(date.getTime()) ? item.created_at : `${date.toLocaleString('zh-CN', {
        timeZone: data.time_zone || 'UTC', hour12: false, timeZoneName: 'shortOffset',
      })} (${data.time_zone || 'UTC'})`);
      time.dateTime = item.created_at;
      article.append(identity, time);
      const text = node('div'); text.className = 'comment-body';
      if (item.placeholder) text.textContent = item.deleted ? '该评论已删除' : '该评论暂不可用';
      else text.innerHTML = item.content_html;
      article.append(text);
      let replies;
      if (!parent) {
        threadAvailable(item.id);
        replies = node('div'); replies.className = 'comment-replies';
        views.set(item.id, { container: replies, page: 1 });
        const show = node('button', '查看回复'); show.type = 'button';
        show.onclick = async () => { show.disabled = true; try { await load(replies, item.id); } catch (e) { threadUnavailable(e, item.id, replies); } finally { show.disabled = false; } };
        article.append(show);
      }
      if (enabled && !item.placeholder) {
        const reply = node('button', '回复'); reply.type = 'button';
        const replyForm = node('div');
        reply.onclick = () => { replyForm.replaceChildren(); form(item.id, replyForm, parent || item.id); };
        if (forms.has(item.id)) form(item.id, replyForm, parent || item.id);
        article.append(reply, replyForm);
      }
      if (replies) article.append(replies);
      container.append(article);
    }
    if (!data.items.length) container.append(node('p', parent ? '暂无已通过的回复。' : '暂无评论，欢迎留下想法。'));
    const nav = node('div'); nav.className = 'comment-pagination';
    for (const [label, target, allowed] of [['上一页', page-1, page>1], ['下一页', page+1, page*20<data.total]]) {
      const button = node('button', label); button.type = 'button'; button.disabled = !allowed;
      button.onclick = async () => { button.disabled = true; try { await load(container, parent, target); } catch (e) { if (parent) threadUnavailable(e, parent, container); else clearUnavailable(e); button.disabled = false; } };
      nav.append(button);
    }
    nav.append(node('span', `第 ${page} 页`)); container.append(nav);
  }
  refreshComments.onclick = async () => {
    refreshComments.disabled = true;
    try { await load(list, null, views.get(null)?.page || 1); }
    catch (e) { clearUnavailable(e); }
    finally { refreshComments.disabled = false; }
  };
  refreshIdentity().catch(() => {});
  load(list).catch(clearUnavailable);
})();
