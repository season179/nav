import {
  Sidebar,
  SidebarContent,
  SidebarHeader,
  SidebarRail,
} from "@/components/ui/sidebar";

export function AppSidebar() {
  return (
    <Sidebar>
      <SidebarHeader className="h-10" />
      <SidebarContent />
      <SidebarRail />
    </Sidebar>
  );
}
