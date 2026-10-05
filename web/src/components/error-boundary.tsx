import { Component, type ErrorInfo, type ReactNode } from "react";
import { useLocation } from "react-router";
import { AuthLayout } from "@/components/auth-layout";
import { Button } from "@/components/ui/button";
import { isChunkLoadError, reloadForNewBundle } from "@/lib/stale-bundle";

type Props = { children: ReactNode; resetKey: string };
type State = { error: Error | null; reloading: boolean; key: string };

// Without a boundary, any render error (a missing lazy chunk included)
// unmounts the whole tree and leaves a blank page.
class Boundary extends Component<Props, State> {
  state: State = { error: null, reloading: false, key: this.props.resetKey };

  // Navigating away clears the error, so one broken page does not stick.
  static getDerivedStateFromProps(props: Props, state: State): Partial<State> | null {
    return props.resetKey === state.key ? null : { error: null, reloading: false, key: props.resetKey };
  }

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    if (isChunkLoadError(error) && reloadForNewBundle()) {
      this.setState({ reloading: true });
      return;
    }
    console.error(error, info.componentStack);
  }

  render() {
    const { error, reloading } = this.state;
    if (!error) return this.props.children;
    if (reloading) return null;
    const stale = isChunkLoadError(error);
    return (
      <AuthLayout
        title={stale ? "isb was updated" : "Something went wrong"}
        description={
          stale ? "This page needs the latest version of the app. Reload to get it." : error.message
        }
      >
        <Button className="w-full" onClick={() => window.location.reload()}>
          Reload
        </Button>
      </AuthLayout>
    );
  }
}

/** Catches render errors under the router, so a failure shows a page with a
 *  Reload button instead of a blank screen. A stale bundle reloads itself. */
export function ErrorBoundary({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  return <Boundary resetKey={pathname}>{children}</Boundary>;
}
