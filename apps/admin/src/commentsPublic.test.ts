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
it('renders server-sanitized HTML while keeping malicious nicknames as text', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:1,items:[{
    id:'root',nickname:'<img src=x onerror=alert(1)>',content_html:'&lt;script&gt;alert(1)&lt;/script&gt;\nSecond line',created_at:'today',is_author:false,
  }]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('.comment-body')?.textContent).toBe('<script>alert(1)</script>\nSecond line'));
  expect(document.querySelector('img,script')).toBeNull();
  expect(document.querySelector('strong')?.textContent).toBe('<img src=x onerror=alert(1)>');
});
it('keeps draft after network failure, shows pending receipt and never publishes optimistically', async()=>{
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
  expect(first).toEqual(second);
  expect(first).not.toHaveProperty('request_id');

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
    {id:'guest',nickname:'Sun · 作者',content_html:'Guest',created_at:'today',is_author:false},
    {id:'author',nickname:'Sun',content_html:'Author',created_at:'today',is_author:true},
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

it('preserves the main draft while paging through comments', async()=>{
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
  expect(JSON.parse(attempts[1][1].body)).not.toHaveProperty('request_id');
});

it('preserves reply drafts when reopening the form and returning from another page', async()=>{
  const firstPage = {enabled:true,total:21,items:[{id:'root',nickname:'Reader',content_html:'Root',created_at:'today',is_author:false}]};
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
  expect(JSON.parse(attempts[1][1].body)).not.toHaveProperty('request_id');
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

it.each([500, 'offline', 'pending'] as const)('loads public comments independently when identity is %s and allows retry', async failure => {
  if (failure === 'pending') fetcher.mockImplementationOnce(() => new Promise(() => {}));
  else if (failure === 'offline') fetcher.mockRejectedValueOnce(new Error('offline'));
  else fetcher.mockResolvedValueOnce(response({}, failure));
  fetcher.mockResolvedValueOnce(response({enabled:true,total:1,items:[
    {id:'root',nickname:'Reader',content_html:'Public comment',created_at:'today'},
  ]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('.comment-body')?.textContent).toBe('Public comment'));
  const form = document.querySelector('form')!;
  form.querySelector('textarea')!.value = '保留正文';
  expect(form.querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(true);
  fireEvent.submit(form);
  expect(fetcher).toHaveBeenCalledTimes(2);
  if (failure === 'pending') return;
  const retry = [...document.querySelectorAll('button')].find(b=>b.textContent==='重试身份校验')!;
  expect(retry.hidden).toBe(false);
  fetcher.mockResolvedValueOnce(response({},401));
  fireEvent.click(retry);
  await waitFor(()=>expect(form.querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(false));
  expect(form.querySelector('textarea')!.value).toBe('保留正文');
  expect(retry.hidden).toBe(true);
});

it.each(['load', 'submit', 'page'])('isolates unavailable threads during %s and preserves every draft', async action => {
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:2,items:[
    {id:'one',nickname:'One',content_html:'First',created_at:'today'},
    {id:'two',nickname:'Two',content_html:'Second',created_at:'today'},
  ]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelectorAll('.comment-item')).toHaveLength(2));
  const [one, two] = [...document.querySelectorAll('.comment-item')];
  for (const article of [one, two]) {
    fireEvent.click([...article.querySelectorAll('button')].find(b=>b.textContent==='回复')!);
  }
  const forms = [...document.querySelectorAll('form')];
  forms.forEach((form, i) => {
    form.querySelector('textarea')!.value = 'draft-' + i;
    form.querySelector('input')!.value = 'Guest';
  });
  const show = [...one.querySelectorAll('button')].find(b=>b.textContent==='查看回复')!;
  if (action === 'page') {
    fetcher.mockResolvedValueOnce(response({enabled:true,total:21,items:[]}));
    fireEvent.click(show);
    await waitFor(()=>expect(one.querySelector('.comment-pagination')).not.toBeNull());
  }
  fetcher.mockResolvedValueOnce(response({error:'评论不存在'},404));
  if (action === 'submit') fireEvent.submit(one.querySelector('form')!);
  else if (action === 'page') fireEvent.click([...one.querySelectorAll('button')].find(b=>b.textContent==='下一页')!);
  else fireEvent.click(show);
  await waitFor(()=>expect(document.body.textContent).toContain('评论不存在'));
  expect(document.querySelectorAll('.comment-item')).toHaveLength(2);
  expect([...document.querySelectorAll('form')]).toEqual(forms);
  forms.forEach((form, i)=>expect(form.querySelector('textarea')!.value).toBe('draft-' + i));
  expect(one.querySelector('form button[type=submit]')!.hasAttribute('disabled')).toBe(true);
  expect(two.querySelector('form button[type=submit]')!.hasAttribute('disabled')).toBe(false);
  expect(document.querySelectorAll('form')[2].querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(false);
});

it.each(['replies', 'root list'])('restores an unavailable reply through %s without losing its draft', async recovery => {
  const firstPage = {enabled:true,total:21,items:[
    {id:'root',nickname:'Reader',content_html:'Root',created_at:'today'},
  ]};
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response(firstPage));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('.comment-item')).not.toBeNull());
  fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='回复')!);
  const form = document.querySelector('.comment-item form')!;
  form.querySelector('input')!.value = 'Guest';
  form.querySelector('textarea')!.value = '恢复后继续提交的草稿';

  fetcher.mockResolvedValueOnce(response({error:'评论不存在'},404));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('该评论已不可用'));
  expect(form.querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(true);
  fireEvent.submit(form);
  expect(fetcher.mock.calls.filter(([,options])=>options?.method==='POST')).toHaveLength(1);

  if (recovery === 'replies') {
    fetcher.mockResolvedValueOnce(response({enabled:true,total:0,items:[]}));
    fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='查看回复')!);
  } else {
    fetcher.mockResolvedValueOnce(response({enabled:true,total:21,items:[]}));
    fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='下一页')!);
    await waitFor(()=>expect(document.body.textContent).toContain('第 2 页'));
    fetcher.mockResolvedValueOnce(response(firstPage));
    fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='上一页')!);
  }
  await waitFor(()=>expect(form.querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(false));
  expect(document.querySelector('.comment-item form')).toBe(form);
  expect(form.querySelector('input')!.value).toBe('Guest');
  expect(form.querySelector('textarea')!.value).toBe('恢复后继续提交的草稿');
  expect(document.body.textContent).not.toContain('该评论已不可用');
  expect(document.body.textContent).not.toContain('评论不存在');

  fetcher.mockResolvedValueOnce(response({message:'已提交，等待审核'},202));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('已提交，等待审核'));
  const attempts = fetcher.mock.calls.filter(([,options])=>options?.method==='POST');
  expect(attempts).toHaveLength(2);
  expect(JSON.parse(attempts[0][1].body)).toEqual(JSON.parse(attempts[1][1].body));
  expect(form.querySelector('textarea')!.value).toBe('');
});

it('keeps a deleted root anonymous and flattens nested replies with their direct target', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:1,items:[
    {id:'root',nickname:'must not display',content_html:'must not display',placeholder:true,deleted:true,is_author:true,created_at:'today'},
  ]}));
  window.eval(script);
  await waitFor(()=>expect(document.body.textContent).toContain('该评论已删除'));
  expect(document.body.textContent).not.toContain('must not display');
  expect(document.querySelector('.comment-author-badge')).toBeNull();
  expect([...document.querySelector('.comment-item')!.querySelectorAll('button')].some(b=>b.textContent==='回复')).toBe(false);
  fetcher.mockResolvedValueOnce(response({enabled:true,total:2,items:[
    {id:'child',root_id:'root',parent_id:'root',parent_nickname:null,nickname:'A',content_html:'<p>Child</p>',created_at:'today'},
    {id:'nested',root_id:'root',parent_id:'child',parent_nickname:'A',nickname:'B',content_html:'<p><strong>Nested</strong></p>',created_at:'today'},
  ]}));
  fireEvent.click([...document.querySelectorAll('button')].find(b=>b.textContent==='查看回复')!);
  await waitFor(()=>expect(document.querySelectorAll('.comment-replies > .comment-item')).toHaveLength(2));
  expect(fetcher.mock.calls[2][0]).toContain('root_id=root');
  expect(document.querySelector('.comment-replies .comment-replies')).toBeNull();
  const nested = document.querySelectorAll('.comment-replies > .comment-item')[1];
  expect(nested.textContent).toContain('回复 A');
  expect(nested.querySelector('.comment-body strong')?.textContent).toBe('Nested');
  fireEvent.click([...nested.querySelectorAll('button')].find(b=>b.textContent==='回复')!);
  const form = nested.querySelector('form')!;
  form.querySelector('input')!.value='Guest'; form.querySelector('textarea')!.value='Fourth level';
  fetcher.mockResolvedValueOnce(response({message:'已提交，等待审核'},202));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('已提交，等待审核'));
  expect(JSON.parse(fetcher.mock.calls[3][1].body).parent_id).toBe('nested');
});

it('previews with the server renderer and preserves optional private email', async()=>{
  fetcher.mockResolvedValueOnce(response({},401)).mockResolvedValueOnce(response({enabled:true,total:0,items:[]}));
  window.eval(script);
  await waitFor(()=>expect(document.querySelector('form')).not.toBeNull());
  const form = document.querySelector('form')!;
  const textarea = form.querySelector('textarea')!;
  form.querySelector('input[name=nickname]')!.setAttribute('value','Guest');
  (form.querySelector('input[name=email]') as HTMLInputElement).value='private@example.com';
  textarea.value='Hello'; textarea.setSelectionRange(0,5);
  fireEvent.click([...form.querySelectorAll('button')].find(b=>b.textContent==='粗体')!);
  expect(textarea.value).toBe('**Hello**');
  fetcher.mockResolvedValueOnce(response({content_html:'<p><strong>Hello</strong></p>'}));
  fireEvent.click([...form.querySelectorAll('button')].find(b=>b.textContent==='预览')!);
  await waitFor(()=>expect(form.querySelector('.comment-preview strong')?.textContent).toBe('Hello'));
  expect(fetcher.mock.calls[2][0]).toBe('/api/v1/comments/preview');
  expect(JSON.parse(fetcher.mock.calls[2][1].body)).toEqual({body:'**Hello**'});
  fetcher.mockResolvedValueOnce(response({message:'已提交，等待审核'},202));
  fireEvent.submit(form);
  await waitFor(()=>expect(form.textContent).toContain('已提交，等待审核'));
  expect(JSON.parse(fetcher.mock.calls[3][1].body).email).toBe('private@example.com');
});
