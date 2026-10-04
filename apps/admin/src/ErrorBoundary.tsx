import { Button, Result } from "antd";
import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { AdminProviders } from "./providers";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/**
 * 顶层渲染兜底：任何 render 期间抛出的异常（例如路由解析、屏幕组件）都在这里
 * 变成可读提示，而不是空白页。React 只会捕获子树渲染错误，事件回调里的异常
 * 仍由各屏幕自己的 try/catch 处理。
 *
 * 这里自己包一层 `AdminProviders`：兜底界面本身也是 antd 组件，
 * 不能假设出错的那个子树已经把 Provider 建好了。
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("后台界面渲染失败", error, info.componentStack);
  }

  render(): ReactNode {
    if (this.state.error === null) return this.props.children;
    return (
      <AdminProviders>
        <Result
          status="error"
          title="界面出错了"
          subTitle={this.state.error.message}
          extra={
            <>
              <p style={{ marginBottom: 16 }}>
                <span>未保存的编辑可能已丢失，返回列表后请重新打开这篇文章。</span>
              </p>
              {/* 用整页跳转而非 SPA 路由：出错后的内存状态不可信，直接重新加载。 */}
              <Button type="primary" href="/admin/">
                返回列表
              </Button>
            </>
          }
        />
      </AdminProviders>
    );
  }
}
