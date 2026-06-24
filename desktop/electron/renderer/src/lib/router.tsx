import {
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import App from "../App.tsx";

const rootRoute = createRootRoute({
  component: App,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
});

const chatRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "chat",
});

const sessionChatRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "sessions/$sessionId",
});

const sessionStacksRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "sessions/$sessionId/stacks",
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "settings",
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  chatRoute,
  sessionChatRoute,
  sessionStacksRoute,
  settingsRoute,
]);

export const router = createRouter({
  history: createHashHistory(),
  routeTree,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
