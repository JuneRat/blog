import { cleanup, render, screen, fireEvent, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { AdminProviders } from '../providers';
import { CommentListScreen } from './CommentListScreen';
import { commentsApi } from '../api';
vi.mock('../auth',()=>({useAuth:()=>({me:{permissions:['post.update']}})}));
vi.mock('../api',()=>({commentsApi:{list:vi.fn(),moderate:vi.fn()}}));
afterEach(()=>{cleanup();vi.clearAllMocks();});
it('renders sanitized comment HTML in moderation and submits the displayed version',async()=>{
  const item={id:'c1',post_id:'p1',post_slug:'post',post_title:'Title',parent_id:null,root_id:null,parent_nickname:null,author_email:'guest@example.com',ip_address:'198.51.100.2',content_html:'<p>&lt;img src=x onerror=alert(1)&gt;<br>Text</p>',nickname:'<script>name</script>',body:'<img src=x onerror=alert(1)>\nText',status:'pending',version:3,is_author:false,created_at:'today'};
  vi.mocked(commentsApi.list).mockResolvedValue({items:[item],total:1,enabled:true});
  vi.mocked(commentsApi.moderate).mockResolvedValue();
  render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}><AdminProviders><CommentListScreen/></AdminProviders></QueryClientProvider>);
  await screen.findByText('<script>name</script>');
  expect(document.querySelector('img,script')).toBeNull();
  fireEvent.click(screen.getByRole('button',{name:'已通过'}));
  await waitFor(()=>expect(commentsApi.moderate).toHaveBeenCalledWith(item,'approved'));
});
