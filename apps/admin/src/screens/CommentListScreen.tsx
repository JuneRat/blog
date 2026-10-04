import { formatDateTime } from "../timeZone";
import { useTimeZone } from "../timeZoneContext";
import { useEffect, useState } from 'react';
import { Alert, App, Button, Card, Checkbox, Modal, Pagination, Select, Space, Tag, Tooltip, Typography } from 'antd';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { commentsApi } from "../api/comments";
import type { CommentBatchActionInput, CommentItem } from "../api/generated";
import { permissionMessageOf } from '../apiError';
import { CommentEditor } from '../components/CommentEditor';
import { paths, navigate } from '../router';
import { queryKeys } from '../queryClient';
import { invalidateAfterWrite } from '../queryEffects';
const PAGE_SIZE = 20;
const labels: Record<string, string> = { pending: '待审核', approved: '已通过', trash: '回收站', spam: '垃圾评论' };
const actions: Record<string, string> = { pending: '退回待审', approved: '通过审核', spam: '标记垃圾', trash: '移入回收站' };
const reasons: Record<string, string> = {
  all_comments: '全站设置要求审核',
  guest: '游客评论需要审核',
  first_comment: '该账号尚无人工审核通过的评论',
  restored: '从垃圾评论或回收站恢复，需重新审核',
  manual_review: '已转为待审核',
};
export function CommentListScreen() {
  const timeZone = useTimeZone();
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
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const query = useQuery({ queryKey: queryKeys.comments(page, status, post), queryFn: () => commentsApi.list(page, status, post) });
  useEffect(() => {
    if (!query.data || query.isFetching || query.isError) return;
    const lastPage = Math.max(1, Math.ceil(query.data.total / query.data.per_page));
    if (page > lastPage) setPage(lastPage);
  }, [page, query.data, query.isFetching, query.isError]);
  useEffect(() => {
    setSelectedIds([]);
  }, [page, status, post]);
  async function moderate(item: CommentItem, next: string) {
    setBusy(true); setError(undefined);
    try { await commentsApi.moderate(item, next); void message.success(next === 'trash' ? '评论已移入回收站，回复仍保留' : '审核状态已保存'); }
    catch (e) { setError(permissionMessageOf(e)); }
    finally { setBusy(false); await invalidateAfterWrite(client, 'comments'); }
  }
  async function batchModerate(action: CommentBatchActionInput) {
    const targets = (query.data?.items ?? []).filter(item => selectedIds.includes(item.id));
    if (targets.length === 0) return;
    setBusy(true); setError(undefined);
    try {
      const result = await commentsApi.batch({
        action,
        items: targets.map(item => ({ id: item.id, expected_version: item.version })),
      });
      void message.success(`已批量处理 ${result.affected} 条评论`);
      setSelectedIds([]);
    } catch (e) {
      setError(permissionMessageOf(e));
      setSelectedIds([]);
    } finally {
      setBusy(false);
      await invalidateAfterWrite(client, 'comments');
    }
  }
  async function submitReply() {
    if (!reply) return;
    setBusy(true); setError(undefined);
    try { const result = await commentsApi.reply(reply, body); void message.success(result.message); setReply(undefined); await invalidateAfterWrite(client, 'comments'); }
    catch(e) { setError(permissionMessageOf(e)); }
    finally { setBusy(false); }
  }
  const items = query.data?.items ?? [];
  const allSelected = items.length > 0 && selectedIds.length === items.length;
  const indeterminate = selectedIds.length > 0 && selectedIds.length < items.length;
  const selectedItems = items.filter(item => selectedIds.includes(item.id));
  const hasSpamOrTrash = selectedItems.some(item => item.status === 'spam' || item.status === 'trash');

  return <Space orientation="vertical" size="large" style={{width:'100%'}}>
    <Typography.Title level={3} style={{ marginTop: 0 }}>评论管理</Typography.Title>
    <Space wrap>
      <Select aria-label="审核状态" value={status} style={{width:150}} onChange={v => {setStatus(v);setPage(1);}}
        options={[{value:'',label:'全部状态'}, ...Object.entries(labels).map(([value,label])=>({value,label}))]} />
      {post && <Tag closable onClose={()=>{setPost(undefined);setPage(1);}}>{postTitle}</Tag>}
      <Button onClick={()=>void query.refetch()}>刷新</Button>
      {items.length > 0 && (
        <Checkbox checked={allSelected} indeterminate={indeterminate}
          onChange={e => setSelectedIds(e.target.checked ? items.map(i => i.id) : [])}>
          全选本页
        </Checkbox>
      )}
      {selectedIds.length > 0 && (
        <Space wrap style={{ background: '#f5f5f5', padding: '2px 8px', borderRadius: 4 }}>
          <Typography.Text strong style={{ fontSize: 12 }}>已选 {selectedIds.length} 条：</Typography.Text>
          {(status === 'pending' || status === '') && (
            <Tooltip title={hasSpamOrTrash ? "选中的评论包含垃圾或回收站评论，请先恢复待审" : undefined}>
              <span>
                <Button
                  size="small"
                  type="primary"
                  disabled={busy || hasSpamOrTrash}
                  onClick={() => void batchModerate('approve')}
                >
                  批量通过
                </Button>
              </span>
            </Tooltip>
          )}
          {(status === 'spam' || status === 'trash' || (status === '' && hasSpamOrTrash)) && (
            <Button size="small" type="primary" disabled={busy} onClick={() => void batchModerate('restore')}>
              批量恢复待审
            </Button>
          )}
          {status === 'approved' && (
            <Button size="small" disabled={busy} onClick={() => void batchModerate('pending')}>
              批量退回待审
            </Button>
          )}
          {status !== 'spam' && (
            <Button size="small" disabled={busy} onClick={() => void batchModerate('spam')}>
              批量标记垃圾
            </Button>
          )}
          {status !== 'trash' && (
            <Button size="small" danger disabled={busy} onClick={() => void batchModerate('trash')}>
              批量移入回收站
            </Button>
          )}
          {hasSpamOrTrash && (status === '' || status === 'pending') && (
            <Typography.Text type="danger" style={{ fontSize: 12 }}>
              含垃圾/回收站评论，须先恢复待审
            </Typography.Text>
          )}
          <Button size="small" type="text" onClick={() => setSelectedIds([])}>取消</Button>
        </Space>
      )}
    </Space>
    {(error || query.error) && <Alert type="error" title={error || permissionMessageOf(query.error)} />}
    {query.isPending && <Typography.Text>正在加载评论…</Typography.Text>}
    {query.data?.items.length === 0 && <Typography.Text type="secondary">当前筛选下没有评论。</Typography.Text>}
    {query.data?.items.map(item => <Card key={item.id} title={<Space wrap><Checkbox checked={selectedIds.includes(item.id)} onChange={e => setSelectedIds(curr => e.target.checked ? [...curr, item.id] : curr.filter(id => id !== item.id))} /><span>{item.nickname}</span>{item.is_author && <Tag>作者</Tag>}<Tag>{labels[item.status]}</Tag>{item.parent_id && <Tag>回复</Tag>}</Space>}>
      <Button type="link" onClick={()=>navigate(paths.editPost(item.post_id))}>{item.post_title || item.post_slug}</Button>
      <Typography.Paragraph type="secondary">{formatDateTime(item.created_at, timeZone)}</Typography.Paragraph>
      {item.status === 'pending' && <Typography.Paragraph type="secondary">待审原因：{reasons[item.moderation_reason ?? ''] ?? '等待人工审核'}</Typography.Paragraph>}
      {item.parent_id && <Typography.Paragraph type="secondary">回复 {item.parent_nickname || '该评论'}</Typography.Paragraph>}
      <div style={{overflowWrap:'anywhere'}} dangerouslySetInnerHTML={{__html:item.content_html}} />
      {(item.author_email || item.ip_address) && <Typography.Paragraph type="secondary">{item.author_email && `邮箱：${item.author_email} `}{item.ip_address && `IP：${item.ip_address}`}</Typography.Paragraph>}
      <Space wrap>
        <Button onClick={()=>{setPost(item.post_id);setPostTitle(item.post_title || item.post_slug);setPage(1);}}>只看此文章</Button>
        {Object.entries(actions).filter(([value]) => value !== item.status && !(value === 'approved' && ['spam','trash'].includes(item.status))).map(([value,label])=><Button key={value} danger={value==='trash'} disabled={busy} onClick={()=>void moderate(item,value)}>{value==='pending' && ['spam','trash'].includes(item.status) ? '恢复到待审核' : label}</Button>)}
        {item.status==='approved' && <Button disabled={busy} onClick={()=>{setReply(item);setBody('');setError(undefined);}}>回复</Button>}
      </Space>
    </Card>)}
    <Pagination current={page} total={query.data?.total ?? 0} pageSize={query.data?.per_page ?? PAGE_SIZE} showSizeChanger={false} onChange={setPage} />
    <Modal title="回复评论" open={!!reply} onCancel={()=>setReply(undefined)} onOk={()=>void submitReply()} confirmLoading={busy} okButtonProps={{disabled:!body.trim()}} okText="提交回复">
      <Typography.Paragraph>{reply?.body}</Typography.Paragraph>
      <CommentEditor key={reply?.id} disabled={busy} value={body} onChange={setBody} />
      <Typography.Paragraph type="secondary">使用当前登录身份；回复提交后按当前审核策略处理。</Typography.Paragraph>
      {error && <Alert type="error" title={error} />}
    </Modal>
  </Space>;
}
