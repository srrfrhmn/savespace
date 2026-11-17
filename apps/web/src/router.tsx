import type { QueryClient } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { meQuery } from "./auth/session";
import { Accounts } from "./screens/accounts";
import { Recover, Setup, SignIn } from "./screens/auth";
import { Budget } from "./screens/budget";
import { Home, Security } from "./screens/home";
import { Deleted, Help, Privacy } from "./screens/info";
import { Recurring } from "./screens/recurring";
import { ReflectionScreen } from "./screens/reflection";
import { ReviewScreen } from "./screens/review";
import { Rules } from "./screens/rules";
import { Settings } from "./screens/settings";
import { Transactions } from "./screens/transactions";

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

const accountsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/accounts",
  beforeLoad: requireSession,
  component: Accounts,
});

const transactionsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/transactions",
  beforeLoad: requireSession,
  component: Transactions,
});

const budgetRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/budget",
  beforeLoad: requireSession,
  component: Budget,
});

const recurringRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/recurring",
  beforeLoad: requireSession,
  component: Recurring,
});

const reflectionRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/reflection",
  beforeLoad: requireSession,
  component: ReflectionScreen,
});

const rulesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/rules",
  beforeLoad: requireSession,
  component: Rules,
});

const reviewRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/review",
  beforeLoad: requireSession,
  component: ReviewScreen,
});

const securityRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings/security",
  beforeLoad: requireSession,
  component: Security,
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings",
  beforeLoad: requireSession,
  component: Settings,
});

const helpRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/help",
  component: Help,
});

const privacyRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/privacy",
  component: Privacy,
});

const deletedRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/deleted",
  component: Deleted,
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
  accountsRoute,
  transactionsRoute,
  rulesRoute,
  budgetRoute,
  recurringRoute,
  reflectionRoute,
  reviewRoute,
  securityRoute,
  settingsRoute,
  helpRoute,
  privacyRoute,
  deletedRoute,
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
