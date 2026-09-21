import { useAuth } from "../auth";

/** 登录页：提供商列表来自公开只读 `/auth/providers`（id/展示名/类型）。 */
export function LoginScreen() {
  const { providers, providersLoaded } = useAuth();

  return (
    <div className="screen login">
      <h1>博客后台</h1>
      <p className="muted">使用已绑定的外部身份登录。</p>

      {!providersLoaded && <p className="muted">正在读取登录方式…</p>}

      {providersLoaded && providers.length === 0 && (
        <p className="error">
          未配置登录提供商：请运维先用 <code>blog oauth add-oidc</code> 或{" "}
          <code>blog oauth add-github</code> 配置并绑定身份。
        </p>
      )}

      <div className="provider-list">
        {providers.map((provider) => (
          <a
            key={provider.id}
            className="button"
            href={`/auth/login?provider=${encodeURIComponent(provider.id)}&next=${encodeURIComponent("/admin/")}`}
          >
            使用 {provider.name} 登录
          </a>
        ))}
      </div>
    </div>
  );
}
