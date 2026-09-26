import script from '../../../crates/interfaces/assets/comments.js?raw';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, waitFor } from '@testing-library/dom';
const response = (data: unknown, status=200) => ({ ok:status<400,status,json:async()=>data });
let fetcher: ReturnType<typeof vi.fn>;
beforeEach(() => {
  document.body.innerHTML='<section data-comments-slug="hello"></section>';
  fetcher=vi.fn();vi.stubGlobal('fetch',fetcher);
});
afterEach(()=>{vi.unstubAllGlobals();document.body.replaceChildren();});
it('renders malicious nicknames and bodies only as text and preserves newlines', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:1,items:[{
    id:'root',nickname:'<img src=x onerror=alert(1)>',body:'<script>alert(1)</script>\nSecond line',created_at:'today',is_author:false,
  }]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('.comment-body')?.textContent).toBe('<script>alert(1)</script>\nSecond line'));
  expect(document.querySelector('img,script')).toBeNull();
  expect(document.querySelector('strong')?.textContent).toBe('<img src=x onerror=alert(1)>');
});
it('reuses request ID after network failure, shows pending receipt and never publishes optimistically', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:0,items:[]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('form')).not.toBeNull());
  (document.querySelector('input') as HTMLInputElement).value='Guest';
  (document.querySelector('textarea') as HTMLTextAreaElement).value='Hello';
  fetcher.mockRejectedValueOnce(new Error('offline'));
  fireEvent.submit(document.querySelector('form')!);
  await waitFor(()=>expect(document.body.textContent).toContain('offline'));
  fetcher.mockResolvedValueOnce(response({message:'已提交，等待审核'},202));
  fireEvent.submit(document.querySelector('form')!);
  await waitFor(()=>expect(document.body.textContent).toContain('已提交，等待审核'));
  const first=JSON.parse(fetcher.mock.calls[2][1].body as string);
  const second=JSON.parse(fetcher.mock.calls[3][1].body as string);
  expect(first.request_id).toBe(second.request_id);
  expect(first.request_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  expect(document.querySelector('.comment-item')).toBeNull();
});
it('removes discussion and submission form after withdrawn post returns 404', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:21,items:[]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('form')).not.toBeNull());
  fetcher.mockResolvedValueOnce(response({error:'文章不存在'},404));
  const next=[...document.querySelectorAll('button')].find(b=>b.textContent==='下一页')!;
  fireEvent.click(next);
  await waitFor(()=>expect(document.querySelector('form')).toBeNull());
  expect(document.body.textContent).toContain('文章不存在');
});
it('closed discussions show history without a submission form',async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:false,total:0,items:[]}));
  window.eval(script);
  await waitFor(()=>expect(document.body.textContent).toContain('新评论已关闭'));
  expect(document.querySelector('form')).toBeNull();
});

it('renders the trusted author badge separately from visitor-controlled nicknames', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:2,items:[
    {id:'guest',nickname:'Sun · 作者',body:'Guest',created_at:'today',is_author:false},
    {id:'author',nickname:'Sun',body:'Author',created_at:'today',is_author:true},
  ]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelectorAll('.comment-item')).toHaveLength(2));
  const [guest, author] = document.querySelectorAll('.comment-item');
  expect(guest.querySelector('strong')?.textContent).toBe('Sun · 作者');
  expect(guest.querySelector('.comment-author-badge')).toBeNull();
  expect(author.querySelector('strong')?.textContent).toBe('Sun');
  expect(author.querySelector('.comment-author-badge')?.getAttribute('aria-label')).toBe('文章作者');
  expect(author.querySelector('strong .comment-author-badge')).toBeNull();
});

it('preserves the main draft and retry ID while paging through comments', async()=>{
  const page = {enabled:true,total:21,items:[]};
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response(page));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('form')).not.toBeNull());
  const form = document.querySelector('form')!;
  const nickname = form.querySelector('input')!;
  const body = form.querySelector('textarea')!;
  nickname.value = 'Guest'; body.value = '需要保留的评论草稿';
  fetcher.mockRejectedValueOnce(new Error('offline'));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('offline'));

  fetcher.mockResolvedValueOnce(response(page));
  fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='下一页')!);
  await waitFor(()=>expect(document.body.textContent).toContain('第 2 页'));
  expect(document.querySelector('form')).toBe(form);
  expect(nickname.value).toBe('Guest');
  expect(body.value).toBe('需要保留的评论草稿');
  fetcher.mockResolvedValueOnce(response({message:'已提交，等待审核'},202));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('已提交，等待审核'));
  const attempts = fetcher.mock.calls.filter(([,options])=>options?.method==='POST');
  expect(attempts).toHaveLength(2);
  expect(JSON.parse(attempts[0][1].body).request_id).toBe(JSON.parse(attempts[1][1].body).request_id);
});

it('preserves reply drafts when reopening the form and returning from another page', async()=>{
  const firstPage = {enabled:true,total:21,items:[{id:'root',nickname:'Reader',body:'Root',created_at:'today',is_author:false}]};
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response(firstPage));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('.comment-item')).not.toBeNull());
  const replyButton = [...document.querySelectorAll('button')].find(b=>b.textContent==='回复')!;
  fireEvent.click(replyButton);
  const form = document.querySelector('.comment-item form')!;
  form.querySelector('input')!.value = 'Guest';
  form.querySelector('textarea')!.value = '回复草稿';
  fireEvent.click(replyButton);
  expect(document.querySelector('.comment-item form')).toBe(form);
  expect(form.querySelector('textarea')!.value).toBe('回复草稿');

  fetcher.mockResolvedValueOnce(response({enabled:true,total:21,items:[]}));
  fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='下一页')!);
  await waitFor(()=>expect(document.body.textContent).toContain('第 2 页'));
  fetcher.mockResolvedValueOnce(response(firstPage));
  fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='上一页')!);
  await waitFor(()=>expect(document.querySelector('.comment-item form')).toBe(form));
  expect(form.querySelector('input')!.value).toBe('Guest');
  expect(form.querySelector('textarea')!.value).toBe('回复草稿');
});

it('keeps the draft on session expiry and waits for explicit guest resubmission', async()=>{
  fetcher.mockResolvedValueOnce(response({display_name:'Author',csrf_token:'old-token'}))
    .mockResolvedValueOnce(response({enabled:true,total:0,items:[]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('form')).not.toBeNull());
  const form = document.querySelector('form')!;
  const nickname = form.querySelector('input')!;
  const body = form.querySelector('textarea')!;
  body.value = '登录过期时也应保留';
  fetcher.mockResolvedValueOnce(response({error:'未登录'},401)).mockResolvedValueOnce(response({},401));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('请确认昵称后再次提交'));
  expect(body.value).toBe('登录过期时也应保留');
  expect(nickname.disabled).toBe(false);
  expect(fetcher.mock.calls.filter(([,options])=>options?.method==='POST')).toHaveLength(1);

  fetcher.mockResolvedValueOnce(response({message:'已提交，等待审核'},202));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('已提交，等待审核'));
  const attempts = fetcher.mock.calls.filter(([,options])=>options?.method==='POST');
  expect(attempts).toHaveLength(2);
  expect(attempts[0][1].headers['X-CSRF-Token']).toBe('old-token');
  expect(attempts[1][1].headers['X-CSRF-Token']).toBeUndefined();
  expect(JSON.parse(attempts[0][1].body).request_id).toBe(JSON.parse(attempts[1][1].body).request_id);
});

it('does not switch to a guest identity when rechecking the session fails', async()=>{
  fetcher.mockResolvedValueOnce(response({display_name:'Author',csrf_token:'old-token'}))
    .mockResolvedValueOnce(response({enabled:true,total:0,items:[]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('form')).not.toBeNull());
  const form = document.querySelector('form')!;
  form.querySelector('textarea')!.value = '保留正文';
  fetcher.mockResolvedValueOnce(response({error:'未登录'},401)).mockResolvedValueOnce(response({},500));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('身份校验失败'));
  expect(form.querySelector('input')!.disabled).toBe(true);
  expect(form.querySelector('textarea')!.value).toBe('保留正文');
  expect(fetcher.mock.calls.filter(([,options])=>options?.method==='POST')).toHaveLength(1);
});
