import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { lazy, StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";
import { createNavQueryClient } from "./lib/query-client.ts";
import { router } from "./lib/router.tsx";
import "../styles.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("Root element #root not found");
}

const root = createRoot(container);
const queryClient = createNavQueryClient();
const QueryDevtools = import.meta.env.DEV
  ? lazy(async () => {
      const { ReactQueryDevtools } = await import(
        "@tanstack/react-query-devtools"
      );
      return { default: ReactQueryDevtools };
    })
  : null;
const RouterDevtools = import.meta.env.DEV
  ? lazy(async () => {
      const { TanStackRouterDevtools } = await import(
        "@tanstack/react-router-devtools"
      );
      return { default: TanStackRouterDevtools };
    })
  : null;

root.render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
      {QueryDevtools ? (
        <Suspense fallback={null}>
          <QueryDevtools initialIsOpen={false} />
        </Suspense>
      ) : null}
      {RouterDevtools ? (
        <Suspense fallback={null}>
          <RouterDevtools initialIsOpen={false} router={router} />
        </Suspense>
      ) : null}
    </QueryClientProvider>
  </StrictMode>,
);
