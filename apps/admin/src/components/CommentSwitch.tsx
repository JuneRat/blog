import { Alert, Space, Switch, Typography } from 'antd';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commentsApi } from '../api';
import { permissionMessageOf } from '../apiError';

export function CommentSwitch({ post }: { post?: string }) {
  const client = useQueryClient();
  const key = ['comment-policy', post ?? 'global'];
  const query = useQuery({ queryKey: key, queryFn: () => commentsApi.policy(post) });
  const mutation = useMutation({
    mutationFn: (enabled: boolean) => commentsApi.savePolicy({ enabled, version: query.data!.version }, post),
    onSuccess: (data) => { client.setQueryData(key, data); },
    onError: () => { void client.invalidateQueries({ queryKey: key }); },
  });
  return <Space orientation="vertical">
    <Space><Switch aria-label={post ? '允许此文章新评论' : '允许全站新评论'} checked={query.data?.enabled ?? false}
      disabled={!query.data} loading={query.isPending || mutation.isPending} onChange={(value) => mutation.mutate(value)} />
      <Typography.Text>{post ? '允许此文章新评论' : '允许全站新评论'}</Typography.Text></Space>
    <Typography.Text type="secondary">立即保存；关闭后保留已通过的历史评论及回复。{post && '全站关闭时，此开关不生效。'}</Typography.Text>
    {(query.error || mutation.error) && <Alert type="error" title={permissionMessageOf(query.error || mutation.error)} />}
  </Space>;
}
