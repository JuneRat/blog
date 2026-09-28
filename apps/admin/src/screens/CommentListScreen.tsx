import { formatDateTime, useTimeZone } from "../timeZone";
import { useState } from 'react';
import { Alert, App, Button, Card, Modal, Pagination, Select, Space, Tag, Typography } from 'antd';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { commentsApi, type CommentItem } from '../api';
import { permissionMessageOf } from '../apiError';
import { useAuth } from '../auth';
import { CommentEditor } from '../components/CommentEditor';
import { CommentSwitch } from '../components/CommentSwitch';
import { paths, navigate } from '../router';
import { queryKeys } from '../queryClient';
import { invalidateAfterWrite } from '../queryEffects';
const labels: Record<string, string> = { pending: '待审核', approved: '已通过', trash: '回收站', spam: '垃圾评论' };
export function CommentListScreen() {
  const timeZone = useTimeZone();
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
  const query = useQuery({ queryKey: queryKeys.comments(page, status, post), queryFn: () => commentsApi.list(page, status, post) });
  async function moderate(item: CommentItem, next: string) {
    setBusy(true); setError(undefined);
    try { await commentsApi.moderate(item, next); void message.success(next === 'trash' ? '评论已移入回收站，回复仍保留' : '审核状态已保存'); }
    catch (e) { setError(permissionMessageOf(e)); }
    finally { setBusy(false); await invalidateAfterWrite(client, 'comments'); }
  }
  async function submitReply() {
    if (!reply) return;
    setBusy(true); setError(undefined);
    try { const result = await commentsApi.reply(reply, body); void message.success(result.message); setReply(undefined); await invalidateAfterWrite(client, 'comments'); }
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
      <Typography.Paragraph type="secondary">{formatDateTime(item.created_at, timeZone)}</Typography.Paragraph>
      {item.parent_id && <Typography.Paragraph type="secondary">回复 {item.parent_nickname || '该评论'}</Typography.Paragraph>}
      <div style={{overflowWrap:'anywhere'}} dangerouslySetInnerHTML={{__html:item.content_html}} />
      {(item.author_email || item.ip_address) && <Typography.Paragraph type="secondary">{item.author_email && `邮箱：${item.author_email} `}{item.ip_address && `IP：${item.ip_address}`}</Typography.Paragraph>}
      <Space wrap>
        <Button onClick={()=>{setPost(item.post_id);setPostTitle(item.post_title || item.post_slug);setPage(1);}}>只看此文章</Button>
        {Object.entries(labels).filter(([value]) => value !== item.status && !(value === 'approved' && ['spam','trash'].includes(item.status))).map(([value,label])=><Button key={value} danger={value==='trash'} disabled={busy} onClick={()=>void moderate(item,value)}>{value==='pending' && ['spam','trash'].includes(item.status) ? '恢复到待审核' : value==='trash' ? '移入回收站' : label}</Button>)}
        {item.status==='approved' && <Button disabled={busy} onClick={()=>{setReply(item);setBody('');setError(undefined);}}>回复</Button>}
      </Space>
    </Card>)}
    <Pagination current={page} total={query.data?.total ?? 0} pageSize={20} showSizeChanger={false} onChange={setPage} />
    <Modal title="回复评论" open={!!reply} onCancel={()=>setReply(undefined)} onOk={()=>void submitReply()} confirmLoading={busy} okButtonProps={{disabled:!body.trim()}} okText="提交审核">
      <Typography.Paragraph>{reply?.body}</Typography.Paragraph>
      <CommentEditor key={reply?.id} disabled={busy} value={body} onChange={setBody} />
      <Typography.Paragraph type="secondary">使用当前登录身份；回复提交后进入待审核列表。</Typography.Paragraph>
      {error && <Alert type="error" title={error} />}
    </Modal>
  </Space>;
}
