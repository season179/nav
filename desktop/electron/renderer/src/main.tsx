import { QueryClientProvider } from "@tanstack/react-query";
import { lazy, StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";
import App from "./App.tsx";
import { createNavQueryClient } from "./lib/query-client.ts";
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

root.render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
      {QueryDevtools ? (
        <Suspense fallback={null}>
          <QueryDevtools initialIsOpen={false} />
        </Suspense>
      ) : null}
    </QueryClientProvider>
  </StrictMode>,
);
