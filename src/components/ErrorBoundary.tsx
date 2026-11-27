import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/** 全局 / 路由级错误边界：避免单个页面渲染异常导致整个桌面窗口白屏 */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[ErrorBoundary]", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-full items-center justify-center p-8">
          <div className="max-w-md rounded-xl border border-rose-800/70 bg-slate-900 p-6 text-sm text-slate-200 shadow-xl">
            <h2 className="text-base font-medium text-rose-300">页面渲染出现异常</h2>
            <p className="mt-2 break-all font-mono text-xs text-slate-400">
              {this.state.error.message || String(this.state.error)}
            </p>
            <div className="mt-4 flex gap-2">
              <button
                type="button"
                onClick={() => this.setState({ error: null })}
                className="rounded-lg bg-sky-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-sky-500"
              >
                重试渲染
              </button>
              <button
                type="button"
                onClick={() => window.location.reload()}
                className="rounded-lg border border-slate-700 px-3 py-1.5 text-xs text-slate-300 hover:bg-slate-800"
              >
                刷新窗口
              </button>
            </div>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
