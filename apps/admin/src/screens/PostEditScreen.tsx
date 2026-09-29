import { Alert, Button, Card, Col, Flex, Form, Input, Row, Tag, Typography } from "antd";
import { useAuth } from "../auth";
import { ContentPreview } from "../components/ContentPreview";
import { ContentConflict, conflictFields } from "../components/ContentConflict";
import {
  ContentLifecycleControls,
  statusLabel,
} from "../components/ContentLifecycleControls";
import { CommentSwitch } from "../components/CommentSwitch";
import { MediaInsertPanel } from "../components/MediaInsertPanel";
import { PostMetadataFields } from "./postEditor/PostMetadataFields";
import { usePostEditor } from "./postEditor/usePostEditor";
import { EMPTY_FORM, toForm } from "./postEditor/form";

export function PostEditScreen({ id }: { id: string | null }) {
  const { me } = useAuth();
  return <PostEditor key={me?.user_id ?? "anonymous"} id={id} />;
}
function PostEditor({ id }: { id: string | null }) {
  const {
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
              {id === null ? "新建草稿" : view.slug}
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
            <Col xs={24} lg={16} xl={17}>
              <Form.Item label="标题" name="title" style={{ marginBottom: 16 }}>
                <Input
                  size="large"
                  placeholder="输入文章标题…"
                  style={{ fontSize: 18, fontWeight: 600 }}
                />
              </Form.Item>
              <Form.Item label="正文（Markdown）" name="content" style={{ marginBottom: 16 }}>
                <Input.TextArea
                  ref={attachContentRef}
                  rows={22}
                  style={{
                    fontFamily:
                      'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
                    fontSize: 14,
                    lineHeight: 1.6,
                  }}
                  placeholder="在此输入 Markdown 正文…"
                  onDrop={(event) => {
                    const files = Array.from(event.dataTransfer.files);
                    if (files.length === 0) return;
                    event.preventDefault();
                    void insertion.insertFiles(files);
                  }}
                  onPaste={(event) => {
                    const files = Array.from(event.clipboardData?.files ?? []);
                    if (files.length === 0) return;
                    event.preventDefault();
                    void insertion.insertFiles(files);
                  }}
                />
              </Form.Item>

              {canReadMedia && (
                <Flex gap={12} align="center" style={{ marginBottom: 16 }}>
                  <Button
                    type="link"
                    style={{ paddingInline: 0 }}
                    onClick={() => {
                      insertion.clear();
                      setMediaOpen((open) => !open);
                    }}
                  >
                    {mediaOpen ? "收起图片面板" : "插入图片"}
                  </Button>
                  <Typography.Text type="secondary">
                    也可以把图片拖入正文框，或在正文框内粘贴剪贴板图片。
                  </Typography.Text>
                </Flex>
              )}
              {insertion.error !== null && (
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
              {mediaOpen && canReadMedia && (
                <div style={{ marginBottom: 16 }}>
                  <MediaInsertPanel
                    insertion={insertion}
                    canUpload={canUploadMedia}
                    onClose={() => setMediaOpen(false)}
                  />
                </div>
              )}

              <ContentPreview
                key={id ?? "new"}
                content={view.content}
                disabled={formMismatch || busy}
              />
            </Col>

            <Col xs={24} lg={8} xl={7}>
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
                        ? "更新已发布内容"
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
