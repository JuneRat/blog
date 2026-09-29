import { cleanup, render, screen, fireEvent, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { AdminProviders } from '../providers';
import { CommentListScreen } from './CommentListScreen';
import { commentsApi } from '../api';
const item={id:'c1',post_id:'p1',post_slug:'post',post_title:'Title',parent_id:null,root_id:null,parent_nickname:null,author_email:'guest@example.com',ip_address:'198.51.100.2',content_html:'<p>&lt;img src=x onerror=alert(1)&gt;<br>Text</p>',nickname:'<script>name</script>',body:'<img src=x onerror=alert(1)>\nText',status:'pending',moderation_reason:'first_comment',version:3,is_author:false,created_at:'today'};
vi.mock('../auth',()=>({useAuth:()=>({me:{permissions:['post.update']}})}));
vi.mock('../api',()=>({commentsApi:{list:vi.fn(),moderate:vi.fn()}}));
afterEach(()=>{cleanup();vi.clearAllMocks();});
it('renders sanitized comment HTML in moderation and submits the displayed version',async()=>{
  vi.mocked(commentsApi.list).mockResolvedValue({items:[item],total:1,enabled:true});
  vi.mocked(commentsApi.moderate).mockResolvedValue();
  render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}><AdminProviders><CommentListScreen/></AdminProviders></QueryClientProvider>);
  await screen.findByText('<script>name</script>');
  expect(screen.getByText('待审原因：该账号尚无人工审核通过的评论')).toBeTruthy();
  fireEvent.click(screen.getByRole('button',{name:'通过审核'}));
  await waitFor(()=>expect(commentsApi.moderate).toHaveBeenCalledWith(item,'approved'));
});

it('returns to the remaining comments after moderating the last item on the final page', async () => {
  const firstPage = Array.from({ length: 20 }, (_, index) => ({
    ...item, id: `c${index + 1}`, nickname: `待审评论 ${index + 1}`,
  }));
  const last = { ...item, id: 'c21', nickname: '最后一条待审' };
  let moderated = false;
  vi.mocked(commentsApi.list).mockImplementation(async page => ({
    items: page === 1 ? firstPage : moderated ? [] : [last],
    total: moderated ? 20 : 21,
    enabled: true,
  }));
  vi.mocked(commentsApi.moderate).mockImplementation(async () => { moderated = true; });
  render(<AdminProviders><CommentListScreen /></AdminProviders>);
  await screen.findByText('待审评论 1');
  fireEvent.click(screen.getByTitle('下一页'));
  await screen.findByText('最后一条待审');
  fireEvent.click(screen.getByRole('button', { name: '通过审核' }));
  await screen.findByText('待审评论 1');
  expect(screen.queryByText('当前筛选下没有评论。')).toBeNull();
});
