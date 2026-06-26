const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

test("renderer uses shadcn neutral dark tokens without legacy feature CSS", () => {
  const manifest = fs.readFileSync(
    path.join(__dirname, "..", "desktop", "electron", "renderer", "styles.css"),
    "utf8",
  );
  const styles = readRendererStyles();

  assert.match(manifest, /@import "\.\/styles\/theme\.css"/);
  assert.match(manifest, /@import "\.\/styles\/base\.css"/);
  assert.doesNotMatch(
    manifest,
    /@import "\.\/styles\/(sidebar|layout|stacks|settings|transcript|composer)\.css"/,
  );

  assert.match(styles, /:root\s*\{[\s\S]*--background:\s*oklch\(1 0 0\);/);
  assert.match(
    styles,
    /\.dark\s*\{[\s\S]*--background:\s*oklch\(0\.145 0 0\);/,
  );
  assert.match(
    styles,
    /\.dark\s*\{[\s\S]*--sidebar-primary:\s*oklch\(0\.488 0\.243 264\.376\);/,
  );
  assert.match(styles, /--color-chart-1:\s*var\(--chart-1\);/);
  assert.match(styles, /color-scheme:\s*light;/);
  assert.match(styles, /color-scheme:\s*dark;/);
  assert.doesNotMatch(
    styles,
    /\.(sidebar|new-chat|composer|session-view-tab|settings-page|stacks-page)\s*\{/,
  );
  assert.doesNotMatch(styles, /\.app\s*\{/);
});

function readRendererStyles() {
  const entry = path.join(
    __dirname,
    "..",
    "desktop",
    "electron",
    "renderer",
    "styles.css",
  );
  return resolveCssImports(entry);
}

// styles.css is an @import manifest; the rules live in feature partials. Inline
// the imports in order (mirroring how Vite bundles them) so assertions can match
// a rule wherever it lives. Package imports (e.g. "tailwindcss") are left as-is.
function resolveCssImports(filePath: string, seen: Set<string> = new Set()) {
  if (seen.has(filePath)) {
    return "";
  }
  seen.add(filePath);
  const source = fs.readFileSync(filePath, "utf8");
  const dir = path.dirname(filePath);
  return source.replace(
    /@import\s+["']([^"']+)["'];?/g,
    (_match: string, ref: string) => {
      // Skip npm package imports — they are not file paths.
      if (!ref.startsWith("./") && !ref.startsWith("../")) {
        return _match;
      }
      return resolveCssImports(path.resolve(dir, ref), seen);
    },
  );
}
