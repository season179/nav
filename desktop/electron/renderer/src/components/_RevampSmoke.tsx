import { MessageResponse } from "@/components/ai-elements/message";
import { Button } from "@/components/ui/button";

const markdownSmoke = `# AI Elements markdown

- Streamdown list item
- Shiki should highlight the fenced code block

\`\`\`ts
const proof: string = "Electron markdown smoke";
\`\`\``;

// AI-ELEMENTS-SMOKE-OK
export function RevampSmoke() {
  return (
    <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 text-card-foreground">
      <p className="text-sm text-muted-foreground">revamp smoke</p>
      <Button>Tailwind + shadcn OK</Button>
      <div className="rounded-md border bg-background p-3">
        <MessageResponse>{markdownSmoke}</MessageResponse>
      </div>
    </div>
  );
}
