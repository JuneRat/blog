import { useEffect } from 'react';
import { Alert, Select, Space, Switch, Typography } from 'antd';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commentsApi } from '../api';
import { permissionMessageOf } from '../apiError';
import { queryKeys } from '../queryClient';
import { invalidateAfterWrite } from '../queryEffects';
import type { CommentPolicy } from '../api/responseTypes';

const moderationOptions = [
  { value: 'all', label: '全部审核' },
  { value: 'guests', label: '仅游客审核' },
  { value: 'first_comment', label: '首次评论审核' },
  { value: 'none', label: '无需审核' },
];
const moderationHelp = {
  all: '所有新评论和回复都需要人工审核。',
  guests: '游客评论需要人工审核，登录账号的评论直接发布。',
  first_comment: '账号已有人工审核通过且仍保留为通过状态的评论时，后续评论直接发布；游客始终需要审核。',
  none: '所有允许提交的评论和回复直接发布。',
};

export function CommentSwitch({ post, expectedVersion, disabled = false, onSaved, onBusy }: {
  post?: string; expectedVersion?: number | null; disabled?: boolean;
  onSaved?: (version: number, previous: number) => void; onBusy?: (busy: boolean) => void;
}) {
  const client = useQueryClient();
  const key = queryKeys.commentPolicy(post);
  const query = useQuery({ queryKey: key, queryFn: () => commentsApi.policy(post) });
  useEffect(() => { if (post) void client.invalidateQueries({ queryKey: queryKeys.commentPolicy(post) }); }, [client, post, expectedVersion]);
  const mutation = useMutation({
    mutationFn: (policy: CommentPolicy) => commentsApi.savePolicy(policy, post),
    onSuccess: (data, sent) => {
      client.setQueryData(key, data);
      onSaved?.(data.version, sent.version);
      void invalidateAfterWrite(client, post ? 'post' : 'comments');
    },
    onError: () => { void client.invalidateQueries({ queryKey: key }); },
    onSettled: () => { onBusy?.(false); },
  });
  return <Space orientation="vertical">
    <Space><Switch aria-label={post ? '允许此文章评论' : '允许全站评论'} checked={query.data?.enabled ?? false}
      disabled={disabled || mutation.isPending || !query.data || (post !== undefined && expectedVersion == null)} loading={query.isPending || mutation.isPending}
      onChange={(enabled) => { onBusy?.(true); mutation.mutate({ enabled, version: post ? expectedVersion! : query.data!.version }); }} />
      <Typography.Text>{post ? '允许此文章评论' : '允许全站评论'}</Typography.Text></Space>
    <Typography.Text type="secondary">立即保存；关闭后隐藏前台整个评论区域，历史评论及回复仍保留，重新开启后恢复显示。{post && '全站关闭时，此开关不生效。'}</Typography.Text>
    {!post && <>
      <Space><Typography.Text>审核策略</Typography.Text><Select aria-label="审核策略" style={{ minWidth: 180 }}
        value={query.data?.moderation ?? 'all'} options={moderationOptions}
        disabled={disabled || !query.data || mutation.isPending}
        onChange={moderation => mutation.mutate({ ...query.data!, moderation })} /></Space>
      <Typography.Text type="secondary">{moderationHelp[query.data?.moderation ?? 'all']} 修改后立即保存，仅影响新提交，已有待审评论仍需人工处理。</Typography.Text>
    </>}
    {(query.error || mutation.error) && <Alert type="error" title={permissionMessageOf(query.error || mutation.error)} />}
  </Space>;
}
