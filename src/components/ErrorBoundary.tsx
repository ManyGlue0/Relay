import { RotateCcw, TriangleAlert } from "lucide-react";
import { Component, type ReactNode } from "react";

/** If the UI ever trips, say so calmly and offer a reload. Chats and replies
 *  live in the backend, so reloading the window loses nothing. */
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error("Relay UI error", error);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="crash">
        <div className="empty-icon">
          <TriangleAlert size={22} />
        </div>
        <h3>Something in the window went wrong</h3>
        <p>Your chats are safe, and replies keep going in the background. Reload the window to get back to them.</p>
        <code>{this.state.error.message}</code>
        <button className="btn btn-primary" onClick={() => location.reload()}>
          <RotateCcw size={14} />
          Reload window
        </button>
      </div>
    );
  }
}
