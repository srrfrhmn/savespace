import initCore from "@savespace/savespace-core-wasm";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createAppRouter } from "./router";
import "./styles.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing #root element");
}

// Load savespace-core (WASM) once, before the first render. Every money value
// on screen goes through it (design doc §8.4).
await initCore();

const queryClient = new QueryClient({
  defaultOptions: {
    // Financial data: no retry storms, no refetch on every focus.
    queries: { retry: 1, refetchOnWindowFocus: false },
  },
});
const router = createAppRouter(queryClient);

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
