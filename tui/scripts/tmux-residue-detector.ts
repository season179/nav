import {readFileSync} from 'node:fs';

export type TmuxResidueAnalysisInput = {
	capture: string;
	commands: string[];
	finalText: string;
	predictedRows: number;
	wheelBeforeCapture?: string;
	wheelAfterCapture?: string;
	wheelRevealedText?: string;
};

export type TmuxResidueAnalysisResult = {
	ok: boolean;
	failures: string[];
};

type CliOptions = {
	capturePath: string;
	commandPath: string;
	finalText: string;
	predictedRows: number;
	wheelBeforePath?: string;
	wheelAfterPath?: string;
	wheelRevealedText?: string;
};

export function analyzeTmuxResidue(
	input: TmuxResidueAnalysisInput,
): TmuxResidueAnalysisResult {
	const rows = capturedRows(input.capture);
	const failures = [
		...missingCommandFailures(rows, input.commands),
		...residueRowFailures(rows),
		...finalTextFailures(input.capture, input.finalText),
		...rowCountFailures(rows, input.predictedRows),
		...wheelFailures(input),
	];

	return {
		ok: failures.length === 0,
		failures,
	};
}

export function expectedCommandLine(command: string): string {
	return `args command: ${command}`;
}

export function formatResidueFailures(
	failures: string[],
	capture: string,
): string {
	const lines = [
		`FAIL: tmux residue detector found ${failures.length} issue(s)`,
		...failures.map(failure => `- ${failure}`),
		'--- tmux pane ---',
		capture,
	];
	return lines.join('\n');
}

function missingCommandFailures(rows: string[], commands: string[]): string[] {
	const rowSet = new Set(rows);
	const failures: string[] = [];

	for (const command of commands) {
		const expected = expectedCommandLine(command);
		if (!rowSet.has(expected)) {
			failures.push(`missing exact command row: ${expected}`);
		}
	}

	return failures;
}

function residueRowFailures(rows: string[]): string[] {
	const failures: string[] = [];

	for (const row of rows) {
		if (countSubstrings(row, 'command:') > 1) {
			failures.push(`row contains multiple command: substrings: ${row}`);
		}
		if (row.includes('output') && row.includes('command:')) {
			failures.push(`row contains both output and command: ${row}`);
		}
	}

	return failures;
}

function finalTextFailures(capture: string, finalText: string): string[] {
	if (capture.includes(finalText)) {
		return [];
	}
	return [`missing final assistant text: ${finalText}`];
}

function rowCountFailures(rows: string[], predictedRows: number): string[] {
	if (rows.length === predictedRows) {
		return [];
	}
	return [
		`captured row count ${rows.length} did not match predicted ${predictedRows}`,
	];
}

function wheelFailures(input: TmuxResidueAnalysisInput): string[] {
	if (!input.wheelBeforeCapture || !input.wheelAfterCapture) {
		return ['wheel smoke captures were not provided'];
	}

	const failures: string[] = [];
	if (input.wheelBeforeCapture === input.wheelAfterCapture) {
		failures.push('wheel smoke did not change the captured viewport');
	}
	if (
		input.wheelRevealedText &&
		!input.wheelAfterCapture.includes(input.wheelRevealedText)
	) {
		failures.push(
			`wheel smoke did not reveal expected text: ${input.wheelRevealedText}`,
		);
	}

	return failures;
}

function capturedRows(capture: string): string[] {
	return capture.replace(/\n$/, '').split('\n').map(row => row.trim());
}

function countSubstrings(value: string, search: string): number {
	return value.split(search).length - 1;
}

function parseCliOptions(argv: string[]): CliOptions {
	const values = new Map<string, string>();

	for (let index = 0; index < argv.length; index += 2) {
		const key = argv[index];
		const value = argv[index + 1];
		if (!key?.startsWith('--') || value === undefined) {
			throw new Error(`invalid argument near ${key ?? '(end)'}`);
		}
		values.set(key.slice(2), value);
	}

	const capturePath = requiredOption(values, 'capture');
	const commandPath = requiredOption(values, 'commands');
	const finalText = requiredOption(values, 'final-text');
	const predictedRows = Number(requiredOption(values, 'predicted-rows'));
	if (!Number.isInteger(predictedRows) || predictedRows <= 0) {
		throw new Error('--predicted-rows must be a positive integer');
	}

	return {
		capturePath,
		commandPath,
		finalText,
		predictedRows,
		wheelBeforePath: values.get('wheel-before'),
		wheelAfterPath: values.get('wheel-after'),
		wheelRevealedText: values.get('wheel-revealed'),
	};
}

function requiredOption(values: Map<string, string>, name: string): string {
	const value = values.get(name);
	if (!value) {
		throw new Error(`missing --${name}`);
	}
	return value;
}

function readLines(path: string): string[] {
	return readFileSync(path, 'utf8')
		.split(/\r?\n/)
		.map(line => line.trim())
		.filter(Boolean);
}

function readOptionalFile(path: string | undefined): string | undefined {
	if (!path) {
		return undefined;
	}
	return readFileSync(path, 'utf8');
}

function runCli(): void {
	const options = parseCliOptions(process.argv.slice(2));
	const capture = readFileSync(options.capturePath, 'utf8');
	const result = analyzeTmuxResidue({
		capture,
		commands: readLines(options.commandPath),
		finalText: options.finalText,
		predictedRows: options.predictedRows,
		wheelBeforeCapture: readOptionalFile(options.wheelBeforePath),
		wheelAfterCapture: readOptionalFile(options.wheelAfterPath),
		wheelRevealedText: options.wheelRevealedText,
	});

	if (!result.ok) {
		console.error(formatResidueFailures(result.failures, capture));
		process.exitCode = 1;
	}
}

if (import.meta.main) {
	try {
		runCli();
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		console.error(`FAIL: ${message}`);
		process.exitCode = 1;
	}
}
