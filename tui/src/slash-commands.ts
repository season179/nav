export type SlashCommand =
	| {kind: 'exit'}
	| {kind: 'model'}
	| {kind: 'unknown'; name: string};

export function parseSlashCommand(input: string): SlashCommand | null {
	const trimmed = input.trim();
	if (!trimmed.startsWith('/')) {
		return null;
	}

	const [command] = trimmed.slice(1).split(/\s+/, 1);
	const name = command?.toLowerCase() ?? '';

	if (name === 'exit' || name === 'quit') {
		return {kind: 'exit'};
	}
	if (name === 'model') {
		return {kind: 'model'};
	}

	return {kind: 'unknown', name};
}
