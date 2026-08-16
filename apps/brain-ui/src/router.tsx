import {
  Outlet,
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { AppShell } from "./App";
import { AskView } from "./views/AskView";
import { CaptureView } from "./views/CaptureView";
import { ConflictsView } from "./views/ConflictsView";
import { EntityPage } from "./views/EntityPage";
import { GraphView } from "./views/GraphView";
import { MergesView } from "./views/MergesView";
import { Overview } from "./views/Overview";

const rootRoute = createRootRoute({
  component: () => (
    <AppShell>
      <Outlet />
    </AppShell>
  ),
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: Overview,
});

const graphRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/graph",
  component: GraphView,
});

const graphEntityRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/graph/$entityId",
  component: GraphView,
});

const entityRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/entity/$entityId",
  validateSearch: (s: Record<string, unknown>) => ({
    tab: s.tab === "timeline" ? ("timeline" as const) : ("brief" as const),
  }),
  component: EntityPage,
});

const askRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/ask",
  validateSearch: (s: Record<string, unknown>) => ({
    q: typeof s.q === "string" ? s.q : "",
    // Unique-per-press marker; the `/` hotkey pushes a fresh timestamp so
    // AskView's autofocus effect fires on every press, including same-route.
    autofocus: typeof s.autofocus === "number" ? s.autofocus : 0,
  }),
  component: AskView,
});

const conflictsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/conflicts",
  component: ConflictsView,
});

const mergesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/merges",
  component: MergesView,
});

const captureRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/capture",
  component: CaptureView,
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  graphRoute,
  graphEntityRoute,
  entityRoute,
  askRoute,
  conflictsRoute,
  mergesRoute,
  captureRoute,
]);

export const router = createRouter({
  routeTree,
  history: createHashHistory(),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}