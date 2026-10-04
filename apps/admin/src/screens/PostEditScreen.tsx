import { RevisionHistory } from "../components/RevisionHistory";
import { MEDIA_PUBLIC_NOTICE } from "../media";
import { Alert, Button, Card, Col, Flex, Form, Input, Row, Tag, Typography } from "antd";
import { useAuth } from "../auth";
import { MarkdownEditor } from "../components/MarkdownEditor";
import { PublicationPreview } from "../components/PublicationPreview";
import { ContentConflict, conflictFields } from "../components/ContentConflict";
import {
  ContentLifecycleControls,
  statusLabel,
} from "../components/ContentLifecycleControls";
import { CommentSwitch } from "../components/CommentSwitch";
import { MediaInsertDialog } from "../components/MediaInsertDialog";
import { PostMetadataFields } from "./postEditor/PostMetadataFields";
import { usePostEditor } from "./postEditor/usePostEditor";
import { EMPTY_FORM, toForm } from "./postEditor/form";

export function PostEditScreen({ id }: { id: string | null }) {
  const { me } = useAuth();
  return <PostEditor key={me?.user_id ?? "anonymous"} id={id} />;
}
function PostEditor({ id }: { id: string | null }) {
  const {
    hasPendingChanges,
    restoreRevision,
    formApi,
    view,
    version,
    postStatus,
    publishedAt,
    loading,
    busy,
    commentBusy,
    notice,
    error,
    conflict,
    comparison,
    mediaOpen,
    setMediaOpen,
    attachContentRef,
    savedSlug,
    formMismatch,
    unloaded,
    localDraft,
    save,
    reloadFromServer,
    overwriteWithLatest,
    changeStatus,
    canPublish,
    canUnpublish,
    canReadMedia,
    canUploadMedia,
    insertion,
    setCommentBusy,
    onCommentSaved,
  } = usePostEditor(id);
  return (
    <>
      {loading && <Typography.Text type="secondary">正在加载…</Typography.Text>}
      {/* Keep Form mounted so its subscription exists before applying server data. */}
      <div hidden={loading}>
        <Flex
          justify="space-between"
          align="center"
          wrap
          gap={12}
          style={{ marginBottom: 16 }}
        >
          <Flex align="center" gap={8}>
            <Typography.Title level={3} style={{ margin: 0 }}>
              {id === null ? "新建草稿" : (view.title || view.slug)}
            </Typography.Title>
            <Tag color={postStatus === "published" ? "green" : undefined}>
              {statusLabel(postStatus)}
            </Tag>
            {version !== null && (
              <Typography.Text type="secondary">v{version}</Typography.Text>
            )}
          </Flex>
          {postStatus === "published" && id !== null && (
            <Typography.Link
              href={`/posts/${encodeURIComponent(savedSlug)}`}
              target="_blank"
              rel="noreferrer"
            >
              查看公开页面
            </Typography.Link>
          )}
        </Flex>

        {localDraft.panel}
        {hasPendingChanges && <Alert type="info" showIcon title={postStatus === "published" ? "有待发布修改，公开页面仍显示上次发布的内容。" : "服务端修改草稿已保留，当前内容尚未发布。"} style={{ marginBottom: 16 }} />}
        {id !== null && <RevisionHistory key={id} kind="post" id={id} disabled={busy || formMismatch || localDraft.blocksEditing} canRestore={postStatus !== "archived"} onRestore={restoreRevision} />}
        {conflict && (
          <ContentConflict
            fields={conflictFields(
              view,
              comparison.snapshot === null ? null : toForm(comparison.snapshot),
            )}
            version={comparison.snapshot?.version ?? null}
            busy={busy || commentBusy}
            loading={comparison.loading}
            error={comparison.error}
            onRefresh={comparison.refresh}
            onReload={() => void reloadFromServer()}
            onOverwrite={overwriteWithLatest}
          />
        )}

        {notice !== null && (
          <Alert
            type="success"
            showIcon
            title={notice}
            style={{ marginBottom: 16 }}
          />
        )}
        {error !== null && (
          <Alert
            type="error"
            showIcon
            title={error}
            style={{ marginBottom: 16 }}
          />
        )}
        {unloaded && (
          <Alert
            type="warning"
            showIcon
            title="文章未能加载。"
            description="可能已被删除或暂时不可达。已禁用保存与发布，避免把上一次打开的内容写到这个地址。"
            style={{ marginBottom: 16 }}
            action={
              <Button
                disabled={busy || commentBusy}
                onClick={() => void reloadFromServer()}
              >
                重新加载
              </Button>
            }
          />
        )}

        <Form
          form={formApi}
          disabled={
            loading ||
            postStatus === "archived" ||
            formMismatch ||
            localDraft.blocksEditing
          }
          layout="vertical"
          initialValues={EMPTY_FORM}
          onFinish={() => {
            if (!commentBusy) void save();
          }}
        >
          <Row gutter={[24, 24]}>
            <Col xs={24} xl={17}>
              <Form.Item label="标题" name="title" style={{ marginBottom: 16 }}>
                <Input
                  size="large"
                  placeholder="输入文章标题…"
                  style={{ fontSize: 18, fontWeight: 600 }}
                />
              </Form.Item>
              <Form.Item label="正文（Markdown）" name="content" extra={canUploadMedia ? MEDIA_PUBLIC_NOTICE : undefined} style={{ marginBottom: 16 }}>
                <MarkdownEditor
                  editorScope={id ?? "new"}
                  contentRef={attachContentRef}
                  disabled={loading || postStatus === "archived" || formMismatch || localDraft.blocksEditing}
                  onInsertFiles={canUploadMedia ? insertion.insertFiles : undefined}
                  onOpenMedia={canReadMedia || canUploadMedia ? () => { insertion.clear(); setMediaOpen(true); } : undefined}
                  mediaOpen={mediaOpen}
                />
              </Form.Item>

              <PublicationPreview key={id ?? "new"} content={view.content}
                readContent={() => formApi.getFieldValue("content") ?? ""}
                disabled={loading || formMismatch || localDraft.blocksEditing} />

              {!mediaOpen && insertion.error !== null && (
                <Alert
                  type="error"
                  showIcon
                  title={insertion.error}
                  style={{ marginBottom: 16 }}
                />
              )}
              {insertion.notice !== null && (
                <Alert
                  type="success"
                  showIcon
                  title={insertion.notice}
                  style={{ marginBottom: 16 }}
                />
              )}
              {mediaOpen && (canReadMedia || canUploadMedia) && (
                <MediaInsertDialog
                  insertion={insertion}
                  canRead={canReadMedia}
                  canUpload={canUploadMedia}
                  onClose={() => setMediaOpen(false)}
                />
              )}
            </Col>

            <Col xs={24} xl={7}>
              <Flex vertical gap={16} style={{ width: "100%" }}>
                <Card title="发布设置" size="small">
                  <Button
                    type="primary"
                    htmlType="submit"
                    block
                    size="large"
                    disabled={
                      busy ||
                      commentBusy ||
                      formMismatch ||
                      postStatus === "archived" ||
                      localDraft.blocksEditing
                    }
                    style={{ marginBottom: 12 }}
                  >
                    {busy
                      ? "处理中…"
                      : postStatus === "published"
                        ? "保存修改草稿"
                        : postStatus === "scheduled"
                          ? "保存预约内容"
                          : "保存草稿"}
                  </Button>
                  {id !== null && (
                    <Flex justify="center" style={{ marginBottom: 12 }}>
                      <ContentLifecycleControls
                        status={postStatus}
                        publishedAt={publishedAt}
                        disabled={busy || commentBusy || formMismatch}
                        canPublish={canPublish && !localDraft.blocksEditing}
                        canUnpublish={canUnpublish}
                        canArchive={canUnpublish}
                        onAction={changeStatus}
                      />
                    </Flex>
                  )}
                  {id !== null && (
                    <div style={{ paddingTop: 8, borderTop: "1px solid #f0f0f0" }}>
                      <CommentSwitch
                        key={id}
                        post={id}
                        expectedVersion={version}
                        disabled={busy || formMismatch}
                        onBusy={setCommentBusy}
                        onSaved={onCommentSaved}
                      />
                    </div>
                  )}
                </Card>

                <Card title="文章属性" size="small">
                  <PostMetadataFields
                    hideTitle
                    form={formApi}
                    view={view}
                    uploadScope={insertion.uploadScope}
                    canReadMedia={canReadMedia}
                    canUploadMedia={canUploadMedia}
                  />
                </Card>
              </Flex>
            </Col>
          </Row>
        </Form>
      </div>
    </>
  );
}
