import { CaretRightIcon, PlusIcon } from "@phosphor-icons/react";

import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  SidebarRail,
} from "@/components/ui/sidebar";

const placeholderProjects = [
  {
    defaultOpen: false,
    id: "project-placeholder-collapsed",
    label: "Project label",
    sessions: [
      { id: "collapsed-session-a", label: "Chat label" },
      { id: "collapsed-session-b", label: "Chat label" },
    ],
  },
  {
    defaultOpen: true,
    id: "project-placeholder-expanded",
    label: "Project label",
    sessions: [
      { id: "expanded-session-a", label: "Chat label" },
      { id: "expanded-session-b", label: "Chat label" },
      { id: "expanded-session-c", label: "Chat label" },
    ],
  },
];

export function AppSidebar() {
  return (
    <Sidebar>
      <SidebarHeader className="h-10" />
      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupContent>
            <SidebarMenu>
              <SidebarMenuItem>
                <SidebarMenuButton type="button">
                  <PlusIcon data-icon="inline-start" />
                  <span>New chat</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        <SidebarGroup>
          <SidebarGroupLabel>Projects</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu>
              {placeholderProjects.map((project) => (
                <SidebarMenuItem key={project.id}>
                  <Collapsible
                    className="group/collapsible"
                    defaultOpen={project.defaultOpen}
                  >
                    <CollapsibleTrigger asChild>
                      <SidebarMenuButton type="button">
                        <CaretRightIcon
                          className="transition-transform group-data-[state=open]/collapsible:rotate-90"
                          data-icon="inline-start"
                        />
                        <span>{project.label}</span>
                      </SidebarMenuButton>
                    </CollapsibleTrigger>
                    <CollapsibleContent>
                      <SidebarMenuSub>
                        {project.sessions.map((session) => (
                          <SidebarMenuSubItem key={session.id}>
                            <SidebarMenuSubButton href="#">
                              <span>{session.label}</span>
                            </SidebarMenuSubButton>
                          </SidebarMenuSubItem>
                        ))}
                      </SidebarMenuSub>
                    </CollapsibleContent>
                  </Collapsible>
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <SidebarRail />
    </Sidebar>
  );
}
