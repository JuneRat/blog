import { useEffect } from 'react';
import { Alert, Space, Switch, Typography } from 'antd';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commentsApi } from '../api';
import { permissionMessageOf } from '../apiError';

export function CommentSwitch({ post, expectedVersion, disabled = false, onSaved, onBusy }: {
  post?: string; expectedVersion?: number | null; disabled?: boolean;
  onSaved?: (version: number, previous: number) => void; onBusy?: (busy: boolean) => void;
}) {
  const client = useQueryClient();
  const key = ['comment-policy', post ?? 'global'];
  const query = useQuery({ queryKey: key, queryFn: () => commentsApi.policy(post) });
  useEffect(() => { if (post) void client.invalidateQueries({ queryKey: ['comment-policy', post] }); }, [client, post, expectedVersion]);
  const mutation = useMutation({
    mutationFn: (policy: { enabled: boolean; version: number }) => commentsApi.savePolicy(policy, post),
    onSuccess: (data, sent) => { client.setQueryData(key, data); onSaved?.(data.version, sent.version); },
    onError: () => { void client.invalidateQueries({ queryKey: key }); },
    onSettled: () => { onBusy?.(false); },
  });
  return <Space orientation="vertical">
    <Space><Switch aria-label={post ? '允许此文章新评论' : '允许全站新评论'} checked={query.data?.enabled ?? false}
      disabled={disabled || !query.data || (post !== undefined && expectedVersion == null)} loading={query.isPending || mutation.isPending}
      onChange={(enabled) => { onBusy?.(true); mutation.mutate({ enabled, version: post ? expectedVersion! : query.data!.version }); }} />
      <Typography.Text>{post ? '允许此文章新评论' : '允许全站新评论'}</Typography.Text></Space>
    <Typography.Text type="secondary">立即保存；关闭后保留已通过的历史评论及回复。{post && '全站关闭时，此开关不生效。'}</Typography.Text>
    {(query.error || mutation.error) && <Alert type="error" title={permissionMessageOf(query.error || mutation.error)} />}
  </Space>;
}
