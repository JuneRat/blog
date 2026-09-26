import { useState } from 'react';
import { Alert, App, Button, Card, Input, Modal, Pagination, Popconfirm, Select, Space, Tag, Typography } from 'antd';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { commentsApi, commentRequestId, type CommentItem } from '../api';
import { permissionMessageOf } from '../apiError';
import { useAuth } from '../auth';
import { CommentSwitch } from '../components/CommentSwitch';
import { paths, navigate } from '../router';
const labels: Record<string, string> = { pending: '待审核', approved: '已通过', rejected: '已拒绝', spam: '垃圾评论' };
export function CommentListScreen() {
  const { me } = useAuth();
  const { message } = App.useApp();
  const client = useQueryClient();
  const [status, setStatus] = useState('pending');
  const [page, setPage] = useState(1);
  const [post, setPost] = useState<string>();
  const [postTitle, setPostTitle] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [reply, setReply] = useState<CommentItem>();
  const [body, setBody] = useState('');
  const [requestId, setRequestId] = useState('');
  const query = useQuery({ queryKey: ['comments', page, status, post], queryFn: () => commentsApi.list(page, status, post) });
  async function moderate(item: CommentItem, next: string | null) {
    setBusy(true); setError(undefined);
    try { await commentsApi.moderate(item, next); void message.success(next ? '审核状态已保存' : '评论及其回复已删除'); }
    catch (e) { setError(permissionMessageOf(e)); }
    finally { setBusy(false); await client.invalidateQueries({ queryKey: ['comments'] }); }
  }
  async function submitReply() {
    if (!reply) return;
    setBusy(true); setError(undefined);
    try { const result = await commentsApi.reply(reply, body, requestId); void message.success(result.message); setReply(undefined); await client.invalidateQueries({queryKey:['comments']}); }
    catch(e) { setError(permissionMessageOf(e)); }
    finally { setBusy(false); }
  }
  return <Space orientation="vertical" size="large" style={{width:'100%'}}>
    <Typography.Title level={2}>评论管理</Typography.Title>
    {me?.permissions.includes('settings.manage') && <Card><CommentSwitch /></Card>}
    <Space wrap>
      <Select aria-label="审核状态" value={status} style={{width:150}} onChange={v => {setStatus(v);setPage(1);}}
        options={[{value:'',label:'全部状态'}, ...Object.entries(labels).map(([value,label])=>({value,label}))]} />
      {post && <Tag closable onClose={()=>{setPost(undefined);setPage(1);}}>{postTitle}</Tag>}
      <Button onClick={()=>void query.refetch()}>刷新</Button>
    </Space>
    {(error || query.error) && <Alert type="error" title={error || permissionMessageOf(query.error)} />}
    {query.isPending && <Typography.Text>正在加载评论…</Typography.Text>}
    {query.data?.items.length === 0 && <Typography.Text type="secondary">当前筛选下没有评论。</Typography.Text>}
    {query.data?.items.map(item => <Card key={item.id} title={<Space wrap><span>{item.nickname}</span>{item.is_author && <Tag>作者</Tag>}<Tag>{labels[item.status]}</Tag>{item.parent_id && <Tag>回复</Tag>}</Space>}>
      <Button type="link" onClick={()=>navigate(paths.editPost(item.post_id))}>{item.post_title || item.post_slug}</Button>
      <Typography.Paragraph type="secondary">{item.created_at}</Typography.Paragraph>
      <Typography.Paragraph style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere'}}>{item.body}</Typography.Paragraph>
      <Space wrap>
        <Button onClick={()=>{setPost(item.post_id);setPostTitle(item.post_title || item.post_slug);setPage(1);}}>只看此文章</Button>
        {Object.entries(labels).map(([value,label])=><Button key={value} disabled={busy || item.status===value} onClick={()=>void moderate(item,value)}>{label}</Button>)}
        {!item.parent_id && item.status==='approved' && <Button disabled={busy} onClick={()=>{setReply(item);setBody('');setRequestId(commentRequestId());setError(undefined);}}>回复</Button>}
        <Popconfirm title="永久删除此评论及其全部回复？" onConfirm={()=>moderate(item,null)}><Button danger disabled={busy}>删除</Button></Popconfirm>
      </Space>
    </Card>)}
    <Pagination current={page} total={query.data?.total ?? 0} pageSize={20} showSizeChanger={false} onChange={setPage} />
    <Modal title="回复评论" open={!!reply} onCancel={()=>setReply(undefined)} onOk={()=>void submitReply()} confirmLoading={busy} okButtonProps={{disabled:!body.trim()}} okText="提交审核">
      <Typography.Paragraph>{reply?.body}</Typography.Paragraph>
      <Input.TextArea disabled={busy} aria-label="回复正文" value={body} maxLength={2000} showCount rows={5} onChange={e=>{setBody(e.target.value);setRequestId(commentRequestId());}} />
      <Typography.Paragraph type="secondary">使用当前登录身份；回复提交后进入待审核列表。</Typography.Paragraph>
      {error && <Alert type="error" title={error} />}
    </Modal>
  </Space>;
}
