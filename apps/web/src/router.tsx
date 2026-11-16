import type { QueryClient } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { meQuery } from "./auth/session";
import { Recover, Setup, SignIn } from "./screens/auth";
import { Home, Security } from "./screens/home";

type Context = { queryClient: QueryClient };

const rootRoute = createRootRouteWithContext<Context>()({
  component: Outlet,
  notFoundComponent: () => (
    <main className="shell">
      <h1>Page not found</h1>
      <a href="/">Go to home</a>
    </main>
  ),
});

/** Routes that need a session send signed-out people to sign-in. */
async function requireSession({ context }: { context: Context }) {
  const me = await context.queryClient.ensureQueryData(meQuery);
  if (!me) {
    throw redirect({ to: "/sign-in" });
  }
}

/** Sign-in routes send signed-in people home. */
async function requireSignedOut({ context }: { context: Context }) {
  const me = await context.queryClient.ensureQueryData(meQuery);
  if (me) {
    throw redirect({ to: "/" });
  }
}

const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  beforeLoad: requireSession,
  component: Home,
});

const securityRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings/security",
  beforeLoad: requireSession,
  component: Security,
});

const signInRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/sign-in",
  beforeLoad: requireSignedOut,
  component: SignIn,
});

const recoverRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/recover",
  beforeLoad: requireSignedOut,
  component: Recover,
});

const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/setup",
  component: Setup,
});

const routeTree = rootRoute.addChildren([
  homeRoute,
  securityRoute,
  signInRoute,
  recoverRoute,
  setupRoute,
]);

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({ routeTree, context: { queryClient }, defaultPreload: "intent" });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createAppRouter>;
  }
}
