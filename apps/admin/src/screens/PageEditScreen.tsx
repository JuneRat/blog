import { MEDIA_PUBLIC_NOTICE } from "../media";
import { ContentPreview } from "../components/ContentPreview";
import { ContentConflict, conflictFields } from "../components/ContentConflict";
import { ContentLifecycleControls, statusLabel } from "../components/ContentLifecycleControls";
import { Alert, Button, Card, Col, Flex, Form, Input, Row, Select, Tag, Typography } from "antd";
import { useAuth } from "../auth";
import { MediaInsertPanel } from "../components/MediaInsertPanel";
import { usePageEditor } from "./pageEditor/usePageEditor";
import { EMPTY_FORM, toForm } from "./pageEditor/form";

/** Identity changes remount the editor; entity switches are guarded by its controller. */
export function PageEditScreen({ id }: { id: string | null }) {
  const { me } = useAuth();
  return <PageEditor key={me?.user_id ?? "anonymous"} id={id} />;
}

function PageEditor({ id }: { id: string | null }) {
  const {
    formApi,
    view,
    setView,
    version,
    pageStatus,
    publishedAt,
    loading,
    busy,
    notice,
    error,
    conflict,
    comparison,
    deleteConflict,
    mediaOpen,
    setMediaOpen,
    attachContentRef,
    pageId,
    formMismatch,
    unloaded,
    localDraft,
    save,
    reloadFromServer,
    overwriteWithLatest,
    changeStatus,
    deletePage,
    published,
    insertion,
    canReadMedia,
    canUploadMedia,
    canPublish,
    canUnpublish,
    canArchive,
    canDelete,
    savedSlug
  } = usePageEditor(id);

  if (loading) {
    return <Typography.Text type="secondary">正在加载…</Typography.Text>;
  }

  return (
    <>
      <Flex justify="space-between" align="center" wrap gap={12} style={{ marginBottom: 16 }}>
        <Flex align="center" gap={8}>
          <Typography.Title level={3} style={{ margin: 0 }}>
            {id === null ? "新建页面" : view.slug}
          </Typography.Title>
          <Tag color={published ? "green" : undefined}>{statusLabel(pageStatus)}</Tag>
          {version !== null && <Typography.Text type="secondary">v{version}</Typography.Text>}
        </Flex>
        {published && id !== null && (
          <Typography.Link href={`/${encodeURIComponent(savedSlug)}`} target="_blank" rel="noreferrer">
            查看公开页面
          </Typography.Link>
        )}
      </Flex>

      {localDraft.panel}
      {conflict && <ContentConflict
        fields={conflictFields(view, comparison.snapshot === null ? null : toForm(comparison.snapshot))}
        version={comparison.snapshot?.version ?? null} busy={busy} loading={comparison.loading} error={comparison.error}
        onRefresh={comparison.refresh} onReload={() => void reloadFromServer()} onOverwrite={overwriteWithLatest}
      />}

      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}
      {error !== null && (
        <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />
      )}
      {deleteConflict && (
        <Button
          disabled={busy}
          onClick={() => void reloadFromServer()}
          style={{ marginBottom: 16 }}
        >
          重新加载页面
        </Button>
      )}
      {unloaded && (
        <Alert
          type="warning"
          showIcon
          title="页面未能加载。"
          description="可能已被删除或暂时不可达。已禁用保存与发布，避免把上一次打开的内容写到这个地址。"
          style={{ marginBottom: 16 }}
          action={
            <Button disabled={busy} onClick={() => void reloadFromServer()}>
              重新加载
            </Button>
          }
        />
      )}

      <Form
        form={formApi}
        disabled={pageStatus === "archived" || formMismatch || localDraft.blocksEditing}
        layout="vertical"
        initialValues={EMPTY_FORM}
        onValuesChange={(_changed, all) => setView({ ...EMPTY_FORM, ...all })}
        onFinish={() => void save()}
      >
        <Row gutter={[24, 24]}>
          <Col xs={24} lg={16} xl={17}>
            <Form.Item label="标题" name="title" style={{ marginBottom: 16 }}>
              <Input
                size="large"
                placeholder="输入页面标题…"
                style={{ fontSize: 18, fontWeight: 600 }}
              />
            </Form.Item>
            <Form.Item label="正文（Markdown）" name="content" extra={canUploadMedia ? MEDIA_PUBLIC_NOTICE : undefined} style={{ marginBottom: 16 }}>
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
              <Alert type="error" showIcon title={insertion.error} style={{ marginBottom: 16 }} />
            )}
            {insertion.notice !== null && (
              <Alert type="success" showIcon title={insertion.notice} style={{ marginBottom: 16 }} />
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

            <ContentPreview key={id ?? "new"} content={view.content} disabled={formMismatch || busy} />
          </Col>

          <Col xs={24} lg={8} xl={7}>
            <Flex vertical gap={16} style={{ width: "100%" }}>
              <Card title="发布设置" size="small">
                <Button
                  type="primary"
                  htmlType="submit"
                  block
                  size="large"
                  disabled={busy || formMismatch || pageStatus === "archived" || localDraft.blocksEditing}
                  style={{ marginBottom: 12 }}
                >
                  {busy ? "处理中…" : pageStatus === "published" ? "更新已发布内容" : pageStatus === "scheduled" ? "保存预约内容" : "保存草稿"}
                </Button>
                {id !== null && (
                  <Flex justify="center" style={{ marginBottom: 12 }}>
                    <ContentLifecycleControls
                      status={pageStatus}
                      publishedAt={publishedAt}
                      disabled={busy || formMismatch}
                      canPublish={canPublish && !localDraft.blocksEditing}
                      canUnpublish={canUnpublish}
                      canArchive={canArchive}
                      onAction={changeStatus}
                    />
                  </Flex>
                )}
                {canDelete && id !== null && !formMismatch && pageId !== null && (
                  <Button danger block disabled={busy} onClick={deletePage}>
                    移入页面回收站
                  </Button>
                )}
              </Card>

              <Card title="页面属性" size="small">
                <Form.Item label={`slug（公开地址 /${view.slug || "…"}）`} name="slug">
                  <Input placeholder="留空则自动生成（发布后锁定；不能占用 admin、api 等系统路径）" />
                </Form.Item>
                <Form.Item label="可见性" name="visibility">
                  <Select
                    options={[
                      { value: "public", label: "公开" },
                      { value: "private", label: "私有" },
                    ]}
                  />
                </Form.Item>
              </Card>
            </Flex>
          </Col>
        </Row>
      </Form>
    </>
  );
}
