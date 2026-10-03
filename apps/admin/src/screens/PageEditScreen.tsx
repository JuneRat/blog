import { RevisionHistory } from "../components/RevisionHistory";
import { MEDIA_PUBLIC_NOTICE } from "../media";
import { MarkdownEditor } from "../components/MarkdownEditor";
import { PublicationPreview } from "../components/PublicationPreview";
import { ContentConflict, conflictFields } from "../components/ContentConflict";
import { ContentLifecycleControls, statusLabel } from "../components/ContentLifecycleControls";
import { Alert, Button, Card, Col, Flex, Form, Input, Row, Select, Tag, Typography } from "antd";
import { useAuth } from "../auth";
import { MediaInsertDialog } from "../components/MediaInsertDialog";
import { usePageEditor } from "./pageEditor/usePageEditor";
import { EMPTY_FORM, toForm } from "./pageEditor/form";

/** Identity changes remount the editor; entity switches are guarded by its controller. */
export function PageEditScreen({ id }: { id: string | null }) {
  const { me } = useAuth();
  return <PageEditor key={me?.user_id ?? "anonymous"} id={id} />;
}

function PageEditor({ id }: { id: string | null }) {
  const {
    hasPendingChanges,
    restoreRevision,
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
            {id === null ? "新建页面" : (view.title || view.slug)}
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
        {hasPendingChanges && <Alert type="info" showIcon title={pageStatus === "published" ? "有待发布修改，公开页面仍显示上次发布的内容。" : "服务端修改草稿已保留，当前内容尚未发布。"} style={{ marginBottom: 16 }} />}
        {id !== null && <RevisionHistory key={id} kind="page" id={id} disabled={busy || formMismatch || localDraft.blocksEditing} canRestore={pageStatus !== "archived"} onRestore={restoreRevision} />}
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
          <Col xs={24} xl={17}>
            <Form.Item label="标题" name="title" style={{ marginBottom: 16 }}>
              <Input
                size="large"
                placeholder="输入页面标题…"
                style={{ fontSize: 18, fontWeight: 600 }}
              />
            </Form.Item>
            <Form.Item label="正文（Markdown）" name="content" extra={canUploadMedia ? MEDIA_PUBLIC_NOTICE : undefined} style={{ marginBottom: 16 }}>
              <MarkdownEditor
                editorScope={id ?? "new"}
                contentRef={attachContentRef}
                disabled={loading || pageStatus === "archived" || formMismatch || localDraft.blocksEditing}
                onInsertFiles={canUploadMedia ? insertion.insertFiles : undefined}
                onOpenMedia={canReadMedia || canUploadMedia ? () => { insertion.clear(); setMediaOpen(true); } : undefined}
                mediaOpen={mediaOpen}
              />
            </Form.Item>

            <PublicationPreview key={id ?? "new"} content={view.content}
              readContent={() => formApi.getFieldValue("content") ?? ""}
              disabled={loading || formMismatch || localDraft.blocksEditing} />

            {!mediaOpen && insertion.error !== null && (
              <Alert type="error" showIcon title={insertion.error} style={{ marginBottom: 16 }} />
            )}
            {insertion.notice !== null && (
              <Alert type="success" showIcon title={insertion.notice} style={{ marginBottom: 16 }} />
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
                  disabled={busy || formMismatch || pageStatus === "archived" || localDraft.blocksEditing}
                  style={{ marginBottom: 12 }}
                >
                  {busy ? "处理中…" : pageStatus === "published" ? "保存修改草稿" : pageStatus === "scheduled" ? "保存预约内容" : "保存草稿"}
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
