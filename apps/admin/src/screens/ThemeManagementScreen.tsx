import { Alert, App as AntdApp, Button, Card, Col, Flex, Form, Row, Select, Space, Tag, Typography, theme as antdTheme } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../api/client";
import { themeSettingsApi } from "../api/settings";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { ThemeSettings } from "../types";
import type { ThemePackageReport } from "../api/generated";
import { useUnsavedGuard } from "../unsaved";

export function ThemeManagementScreen() {
  const { me } = useAuth();
  const allowed = me?.permissions.includes("settings.manage") ?? false;
  const { token } = antdTheme.useToken();
  const { modal } = AntdApp.useApp();
  const queryClient = useQueryClient();
  const [themeForm] = Form.useForm<{ slug: string }>();
  const [view, setView] = useState<ThemeSettings | null>(null);
  /** 选中主题的渲染镜像，由 `onValuesChange`（同步回调）与 `writeSlug` 维护。 */
  const [slug, setSlug] = useState("");
  const [conflict, setConflict] = useState<ThemeSettings | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [packageFile, setPackageFile] = useState<File | null>(null);
  const [packageReport, setPackageReport] = useState<ThemePackageReport | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  /** 主题设置查询：初次取到写入表单（写操作后由失效重取保持缓存新鲜）。 */
  const themeQuery = useQuery({
    queryKey: queryKeys.themeSettings(),
    queryFn: () => themeSettingsApi.get(),
    enabled: allowed,
  });
  /** 取数失败与写操作失败分开：展示时动作错误优先。 */
  const errorText =
    actionError ?? (themeQuery.error === null ? null : permissionMessageOf(themeQuery.error));
  const setError = setActionError;

  const dirty = packageFile !== null || (view !== null && slug !== view.effective_slug);
  useUnsavedGuard(dirty, "主题管理有未保存的选择或待安装的主题包，离开会丢失。");

  /** 读 store 里当前选中的主题（响应回来时用它判断等待期间是否换了选择）。 */
  const readSlug = useCallback((): string => themeForm.getFieldValue("slug") ?? "", [themeForm]);

  const writeSlug = useCallback(
    (next: string): void => {
      themeForm.setFieldsValue({ slug: next });
      setSlug(next);
    },
    [themeForm],
  );

  useEffect(() => {
    // 只在还没有视图时写入表单：保存响应会与「等待期间换的选择」比对后合并
    // （见 save），无条件写入会把用户的新选择改回去。
    if (themeQuery.data !== undefined && view === null) {
      setView(themeQuery.data);
      writeSlug(themeQuery.data.effective_slug);
    }
  }, [themeQuery.data, view, writeSlug]);

  async function save(expectedVersion: number) {
    const submitted = slug;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await themeSettingsApi.save(submitted, expectedVersion);
      setView(saved);
      // 等待期间换了别的主题：保留用户的新选择，不让响应把它改回去。
      if (readSlug() === submitted) writeSlug(saved.effective_slug);
      setConflict(null);
      // 切换已落库：后台失效重取（不 await，保存响应已是权威值；见站点设置同处说明）。
      void queryClient.invalidateQueries({ queryKey: queryKeys.themeSettings() });
      setNotice(
        `主题已切换为「${saved.available.find((item) => item.slug === saved.slug)?.name ?? saved.slug}」（v${saved.version}），公开页面即刻生效。`,
      );
    } catch (cause) {
      if (cause instanceof ApiError && cause.code === "version_conflict") {
        // 决策用的「此刻服务器值」同样绕开缓存：缓存可能正是冲突的一方。
        try {
          setConflict(await themeSettingsApi.get());
        } catch (reloadError) {
          setError(permissionMessageOf(reloadError));
        }
      } else {
        setError(permissionMessageOf(cause));
      }
    } finally {
      setBusy(false);
    }
  }

  function applyView(next: ThemeSettings) {
    setView(next);
    queryClient.setQueryData(queryKeys.themeSettings(), next);
    const selected = readSlug();
    if (selected === view?.effective_slug || !next.available.some(item => item.slug === selected)) writeSlug(next.effective_slug);
  }

  function clearPackage() {
    setPackageFile(null);
    setPackageReport(null);
    if (fileInput.current !== null) fileInput.current.value = "";
  }

  async function processPackage(install: boolean) {
    if (packageFile === null) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    setPackageReport(null);
    try {
      const report = await (install ? themeSettingsApi.install(packageFile) : themeSettingsApi.validatePackage(packageFile));
      if (install) {
        clearPackage();
        setNotice(`主题「${report.name}」已安装，选择后激活即可生效。`);
        try {
          const next = await themeSettingsApi.get();
          applyView(next);
          if (next.effective_slug === report.slug) setNotice(`主题「${report.name}」已安装，并按原有活动选择生效。`);
        }
        catch (cause) { setError(`安装已完成，但刷新主题列表失败：${permissionMessageOf(cause)}`); }
      } else {
        setPackageReport(report);
        setNotice(`主题包「${report.name}」验证通过，尚未安装。`);
      }
    } catch (cause) { setError(permissionMessageOf(cause)); }
    finally { setBusy(false); }
  }

  async function validateInstalled(selected: string) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const report = await themeSettingsApi.validateInstalled(selected);
      setNotice(`主题「${report.name}」的已安装快照验证通过（${report.template_count} 个模板、${report.asset_count} 个资源）。`);
    } catch (cause) { setError(permissionMessageOf(cause)); }
    finally { setBusy(false); }
  }

  async function remove(selected: string, expectedVersion: number, expectedRelease: string) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      applyView(await themeSettingsApi.uninstall(selected, expectedVersion, expectedRelease));
      setConflict(null);
      setNotice(`主题「${selected}」已卸载。`);
    } catch (cause) {
      setError(permissionMessageOf(cause));
      if (cause instanceof ApiError && cause.code === "version_conflict") {
        try { applyView(await themeSettingsApi.get()); }
        catch (reloadError) { setError(permissionMessageOf(reloadError)); }
      }
    } finally { setBusy(false); }
  }

  async function refresh() {
    setBusy(true);
    setError(null);
    try { applyView(await themeSettingsApi.get()); }
    catch (cause) { setError(permissionMessageOf(cause)); }
    finally { setBusy(false); }
  }

  /** 「仍然覆盖」：二次确认后按服务器最新版本重提。 */
  function overwrite(expectedVersion: number): void {
    modal.confirm({
      title: "仍然覆盖服务器当前主题？",
      content: "公开页面会立即切换。",
      okButtonProps: { danger: true },
      onOk: () => save(expectedVersion),
    });
  }

  if (!allowed) return <><Typography.Title level={3}>主题管理</Typography.Title><Alert type="warning" showIcon title="当前账号没有管理主题的权限。" /></>;

  return (
    <section aria-label="主题管理" style={{ maxWidth: 880 }}>
      <Typography.Title level={3} style={{ marginTop: 0 }}>
        主题管理
      </Typography.Title>
      <Typography.Paragraph type="secondary">
        上传 ZIP 主题包，可先验证再安装。激活后公开页面和样式资源立即生效。
      </Typography.Paragraph>
      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}
      {busy && <Typography.Paragraph role="status" type="secondary">正在处理主题，请稍候…</Typography.Paragraph>}
      {view === null && errorText === null && (
        <Typography.Paragraph type="secondary">正在加载主题…</Typography.Paragraph>
      )}

      {view !== null && (
        <div style={{ marginBottom: 24 }}>
          <Card title="安装主题" style={{ marginBottom: 16 }}>
            <Typography.Paragraph type="secondary">
              ZIP 根目录包含 theme.json、templates 和 assets，也可以放在一个顶层主题目录内。最大 10 MiB。
            </Typography.Paragraph>
            <Space direction="vertical" style={{ width: "100%" }}>
              <Space wrap>
                <Button disabled={busy} onClick={() => fileInput.current?.click()}>选择主题包</Button>
                <Typography.Text type="secondary">{packageFile?.name ?? "未选择主题包"}</Typography.Text>
              </Space>
              <input ref={fileInput} type="file" accept=".zip,application/zip" aria-label="主题 ZIP 包" disabled={busy} style={{ display: "none" }}
                onChange={event => {
                  const file = event.target.files?.[0] ?? null;
                  setPackageReport(null);
                  setNotice(null);
                  if (file !== null && (file.size === 0 || file.size > 10 * 1024 * 1024 || !file.name.toLowerCase().endsWith(".zip"))) {
                    clearPackage();
                    setError("请选择非空且不超过 10 MiB 的 ZIP 主题包。");
                    return;
                  }
                  setError(null);
                  setPackageFile(file);
                }} />
              <Space wrap>
                <Button disabled={busy || packageFile === null} onClick={() => void processPackage(false)}>验证主题包</Button>
                <Button type="primary" disabled={busy || packageFile === null} onClick={() => void processPackage(true)}>安装主题</Button>
                {packageFile !== null && <Button disabled={busy} onClick={clearPackage}>取消选择</Button>}
                <Button disabled={busy} onClick={() => void refresh()}>刷新主题列表</Button>
              </Space>
              {packageReport !== null && <Typography.Text type="secondary">
                {packageReport.slug} · {packageReport.template_count} 个模板 · {packageReport.asset_count} 个资源 · 版本 {packageReport.release.slice(0, 12)}
              </Typography.Text>}
            </Space>
          </Card>
          <Row gutter={[16, 16]}>
            {view.available.map((item) => {
              const isSelected = slug === item.slug;
              const isCurrentActive = view.effective_slug === item.slug;
              const desc =
                item.slug === "default"
                  ? "经典技术博客风格，布局清爽，清晰的代码块与层级排版。"
                  : item.slug === "paper"
                    ? "报纸与杂志社论风格，温润柔和质感与优雅衬线字体，专注沉浸式阅读。"
                    : "已安装主题。";

              return (
                <Col xs={24} sm={12} key={item.slug}>
                  <Card
                    hoverable
                    style={{
                      borderColor: isSelected ? token.colorPrimary : token.colorBorderSecondary,
                      boxShadow: isSelected ? `0 0 0 2px ${token.colorPrimaryBorder}` : undefined,
                    }}
                    styles={{ body: { padding: 16 } }}
                  >
                    <div
                      role="button"
                      aria-label={`选择主题 ${item.name}`}
                      aria-pressed={isSelected}
                      aria-disabled={busy}
                      tabIndex={busy ? -1 : 0}
                      onKeyDown={event => {
                        if (!busy && (event.key === "Enter" || event.key === " ")) {
                          event.preventDefault();
                          writeSlug(item.slug);
                        }
                      }}
                      onClick={() => {
                        if (!busy) {
                          writeSlug(item.slug);
                        }
                      }}
                      style={{
                        cursor: busy ? "not-allowed" : "pointer",
                      }}
                    >
                      <Flex justify="space-between" align="center" style={{ marginBottom: 8 }}>
                        <Typography.Text strong style={{ fontSize: 16 }}>
                          {item.name}
                        </Typography.Text>
                        <Space size={4}>
                          {isCurrentActive && <Tag color="green">当前生效</Tag>}
                          {item.slug === view.fallback_slug && <Tag>启动默认</Tag>}
                          {isSelected && !isCurrentActive && <Tag color="blue">待生效</Tag>}
                        </Space>
                      </Flex>
                      <Typography.Paragraph
                        type="secondary"
                        style={{ fontSize: 13, marginBottom: 12, minHeight: 38 }}
                      >
                        {desc}
                      </Typography.Paragraph>
                      <Flex justify="space-between" align="center">
                        <Typography.Text type="secondary" code style={{ fontSize: 12 }}>
                          {item.slug}
                        </Typography.Text>
                        {isSelected ? (
                          <Typography.Text strong style={{ fontSize: 12, color: token.colorPrimary }}>
                            ✓ 已选择
                          </Typography.Text>
                        ) : (
                          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                            点击选择
                          </Typography.Text>
                        )}
                      </Flex>
                    </div>
                    <Space style={{ marginTop: 12 }}>
                      <Button size="small" aria-label={`验证主题 ${item.name}`} disabled={busy}
                        onClick={() => void validateInstalled(item.slug)}>验证</Button>
                      <Button size="small" danger aria-label={`卸载主题 ${item.name}`}
                        disabled={busy || isCurrentActive || item.slug === view.fallback_slug || item.slug === view.slug}
                        onClick={() => modal.confirm({
                          title: `卸载主题「${item.name}」？`,
                          content: "将删除主题文件和已安装快照。如需恢复，请重新安装主题包。",
                          okText: "卸载", okButtonProps: { danger: true },
                          onOk: () => remove(item.slug, view.version, item.release),
                        })}>卸载</Button>
                    </Space>
                  </Card>
                </Col>
              );
            })}
          </Row>
        </div>
      )}

      <Form
        form={themeForm}
        layout="vertical"
        initialValues={{ slug: "" }}
        onValuesChange={(_changed, all) => setSlug(all.slug ?? "")}
        style={{ maxWidth: 360 }}
      >
        <Form.Item label="选择主题" name="slug">
          <Select
            loading={view === null}
            disabled={busy}
            options={(view?.available ?? []).map((item) => ({
              value: item.slug,
              label: item.name,
            }))}
          />
        </Form.Item>
      </Form>
      {view !== null && (
        <>
          <Typography.Paragraph type="secondary">
            当前主题：
            {view.available.find((item) => item.slug === view.effective_slug)?.name ??
              view.effective_slug}
            （{view.source === "database" ? `数据库 v${view.version}` : "启动配置"}）
          </Typography.Paragraph>
          {view.slug !== view.effective_slug && (
            <Alert
              type="error"
              showIcon
              title={`已保存的主题「${view.slug}」当前未安装，公开页面暂用默认主题。请选择一个已安装主题并保存。`}
              style={{ marginBottom: 16 }}
            />
          )}
          {conflict === null ? (
            <Button
              type="primary"
              disabled={busy || slug === view.slug}
              onClick={() => void save(view.version)}
            >
              激活主题
            </Button>
          ) : (
            <Alert
              type="warning"
              showIcon
              title={`主题已在别处被修改（服务器当前 v${conflict.version}）。`}
              description="你的选择已保留。"
              action={
                <Space>
                  <Button
                    disabled={busy}
                    onClick={() => {
                      // 重新加载：丢弃本地选择，采用服务器值（并同步缓存）。
                      setView(conflict);
                      writeSlug(conflict.effective_slug);
                      queryClient.setQueryData(queryKeys.themeSettings(), conflict);
                      setConflict(null);
                    }}
                  >
                    重新加载
                  </Button>
                  <Button danger disabled={busy} onClick={() => overwrite(conflict.version)}>
                    仍然覆盖
                  </Button>
                </Space>
              }
            />
          )}
        </>
      )}
    </section>
  );
}
