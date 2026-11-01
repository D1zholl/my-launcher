import { Component, type ErrorInfo, type ReactNode } from "react";

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Application render failed", error, info.componentStack);
  }

  render() {
    if (this.state.hasError) {
      return (
        <main className="fatal-error" role="alert">
          <h1>Не удалось открыть экран</h1>
          <p>Перезапустите интерфейс. Данные и файлы инстансов не затронуты.</p>
          <button className="button-primary" type="button" onClick={() => window.location.reload()}>
            Перезапустить интерфейс
          </button>
        </main>
      );
    }
    return this.props.children;
  }
}