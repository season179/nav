export type ComposerDraftStorage = Pick<
  Storage,
  "getItem" | "removeItem" | "setItem"
>;

const COMPOSER_DRAFT_STORAGE_PREFIX = "nav.composerDraft.v1:";
const EMPTY_DRAFT_KEY = "unassigned";

export function browserComposerDraftStorage(): ComposerDraftStorage | null {
  if (typeof window === "undefined") {
    return null;
  }

  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function composerDraftStorageKey(
  draftKey: string | null | undefined,
): string {
  const normalizedKey = draftKey?.trim();
  return `${COMPOSER_DRAFT_STORAGE_PREFIX}${
    normalizedKey ? encodeURIComponent(normalizedKey) : EMPTY_DRAFT_KEY
  }`;
}

export function readComposerDraft(
  storage: ComposerDraftStorage | null | undefined,
  draftKey: string | null | undefined,
): string {
  if (!storage) {
    return "";
  }

  try {
    return storage.getItem(composerDraftStorageKey(draftKey)) ?? "";
  } catch {
    return "";
  }
}

export function writeComposerDraft(
  storage: ComposerDraftStorage | null | undefined,
  draftKey: string | null | undefined,
  draft: string,
): void {
  if (!storage) {
    return;
  }

  try {
    const storageKey = composerDraftStorageKey(draftKey);
    if (draft.length === 0) {
      storage.removeItem(storageKey);
      return;
    }
    storage.setItem(storageKey, draft);
  } catch {
    // Draft persistence is only a convenience; storage failures should not
    // interrupt typing or sending messages.
  }
}
