/** Pure layout checks for the Claude-style composer (rules / prompt / hint). */

export const COMPOSER_ROW_COUNT = 4;

export type ComposerLayoutExpectation = {
	width: number;
	hint: string;
	/** Full second line when input text is shown without a live TextInput cursor. */
	inputLine?: string;
};

export function horizontalRule(width: number): string {
	return '─'.repeat(Math.max(1, width));
}

export function parseComposerFrame(frame: string | undefined): string[] {
	if (!frame) {
		return [];
	}
	return frame.split('\n');
}

/**
 * Asserts the composer matches the fixed row structure:
 * rule → input (starts with `>`) → rule → hint.
 */
export function assertComposerLayout(
	frame: string | undefined,
	expected: ComposerLayoutExpectation,
): void {
	const lines = parseComposerFrame(frame);
	const rule = horizontalRule(expected.width);

	if (lines.length !== COMPOSER_ROW_COUNT) {
		throw new Error(
			`expected ${COMPOSER_ROW_COUNT} lines, got ${lines.length}: ${JSON.stringify(lines)}`,
		);
	}

	if (lines[0] !== rule || lines[2] !== rule) {
		throw new Error(
			`expected full-width horizontal rules (${rule.length} cols), got:\n` +
				`  top: ${JSON.stringify(lines[0])}\n` +
				`  bottom: ${JSON.stringify(lines[2])}`,
		);
	}

	const inputLine = lines[1] ?? '';
	if (!inputLine.startsWith('>')) {
		throw new Error(
			`input line must start with ">", got ${JSON.stringify(inputLine)}`,
		);
	}

	if (expected.inputLine !== undefined && inputLine !== expected.inputLine) {
		throw new Error(
			`input line: want ${JSON.stringify(expected.inputLine)}, got ${JSON.stringify(inputLine)}`,
		);
	}

	if (lines[3] !== expected.hint) {
		throw new Error(
			`hint line: want ${JSON.stringify(expected.hint)}, got ${JSON.stringify(lines[3])}`,
		);
	}
}
