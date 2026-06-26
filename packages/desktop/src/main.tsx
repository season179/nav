import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { AppSidebar } from "@/components/app-sidebar";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";

import "./styles.css";

function App() {
  return (
    <SidebarProvider>
      <AppSidebar />
      <div className="fixed inset-x-0 top-0 z-40 h-10 [-webkit-app-region:drag]" />
      <SidebarTrigger className="fixed top-1 left-[76px] z-50 [-webkit-app-region:no-drag] [&_svg]:!size-[18px]" />
      <SidebarInset />
    </SidebarProvider>
  );
}

const root = document.createElement("div");
root.id = "root";
document.body.replaceChildren(root);

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
