export type ComposerFormValues = {
  message: string;
};

export function normalizeComposerMessage(message: string): string {
  return message.trim();
}

export function validateComposerMessage(
  message: string,
  connected: boolean,
): string | undefined {
  if (!normalizeComposerMessage(message)) {
    return "Message is required";
  }

  if (!connected) {
    return "Backend is not connected";
  }

  return undefined;
}
