import {
  Outlet,
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { AppShell } from "./App";
import { ConflictsView } from "./views/ConflictsView";
import { EntityPage } from "./views/EntityPage";
import { FailuresView } from "./views/FailuresView";
import { MergesView } from "./views/MergesView";
import { OperationsView } from "./views/OperationsView";
import { Overview } from "./views/Overview";
import { SourcesView } from "./views/SourcesView";

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

const entityRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/entity/$entityId",
  validateSearch: (s: Record<string, unknown>) => ({
    tab: s.tab === "timeline" ? ("timeline" as const) : ("brief" as const),
  }),
  component: EntityPage,
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

const failuresRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/failures",
  component: FailuresView,
});

const sourcesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/sources",
  component: SourcesView,
});

const operationsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/operations",
  component: OperationsView,
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  entityRoute,
  conflictsRoute,
  mergesRoute,
  failuresRoute,
  sourcesRoute,
  operationsRoute,
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
