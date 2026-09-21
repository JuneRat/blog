import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

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
      <div className="screen">
        <h1>界面出错了</h1>
        <p className="error">{this.state.error.message}</p>
        <p className="muted">未保存的编辑可能已丢失，返回列表后请重新打开这篇文章。</p>
        <p>
          {/* 用整页跳转而非 SPA 路由：出错后的内存状态不可信，直接重新加载。 */}
          <a className="button" href="/admin/">
            返回列表
          </a>
        </p>
      </div>
    );
  }
}
