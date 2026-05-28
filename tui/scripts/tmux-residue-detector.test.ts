import {describe, expect, test} from 'bun:test';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {
	analyzeTmuxResidue,
	expectedCommandLine,
	formatResidueFailures,
} from './tmux-residue-detector.js';

const commands = [
	'pwd',
	'ls tui/src',
	'rg VirtualHistoryRegion tui/src',
	'bun test',
	'bun run typecheck',
	'git status --short',
];

const finalText = 'Final assistant message: residue check complete.';

describe('tmux residue detector', () => {
	test('accepts a clean captured pane with exact command rows and wheel movement', () => {
		const capture = [
			'Earlier context 09: user asked about #374 residue.',
			...commands.map(command => expectedCommandLine(command)),
			'output',
			'command 6 completed',
			finalText,
			'Enter send',
		].join('\n');
		const beforeWheel = ['visible bottom', expectedCommandLine(commands.at(-1)!)].join(
			'\n',
		);
		const afterWheel = [
			'Earlier context 01: user asked about #374 residue.',
			'visible bottom',
		].join('\n');

		const result = analyzeTmuxResidue({
			capture,
			commands,
			finalText,
			predictedRows: capture.split('\n').length,
			wheelBeforeCapture: beforeWheel,
			wheelAfterCapture: afterWheel,
			wheelRevealedText: 'Earlier context 01: user asked about #374 residue.',
		});

		expect(result.ok).toBe(true);
		expect(result.failures).toEqual([]);
	});

	test('reports every residue contract failure with CI-readable messages', () => {
		const capture = [
			expectedCommandLine(commands[0]!),
			`${expectedCommandLine(commands[1]!)} ${expectedCommandLine(commands[2]!)}`,
			`output ${expectedCommandLine(commands[3]!)}`,
			'Enter send',
		].join('\n');

		const result = analyzeTmuxResidue({
			capture,
			commands,
			finalText,
			predictedRows: 53,
			wheelBeforeCapture: 'same frame',
			wheelAfterCapture: 'same frame',
			wheelRevealedText: 'Earlier context 01: user asked about #374 residue.',
		});

		expect(result.ok).toBe(false);
		expect(result.failures).toContain(
			'missing exact command row: args command: ls tui/src',
		);
		expect(result.failures).toContain(
			'missing exact command row: args command: rg VirtualHistoryRegion tui/src',
		);
		expect(result.failures).toContain(
			'missing exact command row: args command: bun test',
		);
		expect(result.failures).toContain(
			'missing exact command row: args command: bun run typecheck',
		);
		expect(result.failures).toContain(
			'missing exact command row: args command: git status --short',
		);
		expect(result.failures).toContain(
			'row contains multiple command: substrings: args command: ls tui/src args command: rg VirtualHistoryRegion tui/src',
		);
		expect(result.failures).toContain(
			'row contains both output and command: output args command: bun test',
		);
		expect(result.failures).toContain(
			`missing final assistant text: ${finalText}`,
		);
		expect(result.failures).toContain(
			'captured row count 4 did not match predicted 53',
		);
		expect(result.failures).toContain(
			'wheel smoke did not change the captured viewport',
		);
		expect(result.failures).toContain(
			'wheel smoke did not reveal expected text: Earlier context 01: user asked about #374 residue.',
		);
	});

	test('formats failures without hiding the captured pane', () => {
		const result = analyzeTmuxResidue({
			capture: 'bad pane',
			commands,
			finalText,
			predictedRows: 53,
		});
		const formatted = formatResidueFailures(result.failures, 'bad pane');

		expect(formatted).toContain('FAIL: tmux residue detector found 9 issue(s)');
		expect(formatted).toContain('--- tmux pane ---\nbad pane');
	});

	test('reports CLI file errors without a runtime stack trace', () => {
		const tempDir = mkdtempSync(path.join(tmpdir(), 'nav-residue-'));
		const commandPath = path.join(tempDir, 'commands.txt');
		writeFileSync(commandPath, commands.join('\n'));

		const result = Bun.spawnSync({
			cmd: [
				process.execPath,
				path.join(import.meta.dir, 'tmux-residue-detector.ts'),
				'--capture',
				path.join(tempDir, 'missing-pane.txt'),
				'--commands',
				commandPath,
				'--final-text',
				finalText,
				'--predicted-rows',
				'53',
			],
			stderr: 'pipe',
		});

		const stderr = result.stderr.toString();
		rmSync(tempDir, {recursive: true, force: true});

		expect(result.exitCode).toBe(1);
		expect(stderr).toContain('FAIL:');
		expect(stderr).toContain('missing-pane.txt');
		expect(stderr).not.toContain('Bun v');
	});

	test('exits non-zero with detector failures from the CLI', () => {
		const tempDir = mkdtempSync(path.join(tmpdir(), 'nav-residue-'));
		const commandPath = path.join(tempDir, 'commands.txt');
		const capturePath = path.join(tempDir, 'pane.txt');
		writeFileSync(commandPath, commands.join('\n'));
		writeFileSync(capturePath, 'bad pane\n');

		const result = Bun.spawnSync({
			cmd: [
				process.execPath,
				path.join(import.meta.dir, 'tmux-residue-detector.ts'),
				'--capture',
				capturePath,
				'--commands',
				commandPath,
				'--final-text',
				finalText,
				'--predicted-rows',
				'53',
				'--wheel-before',
				capturePath,
				'--wheel-after',
				capturePath,
				'--wheel-revealed',
				'Earlier context 01: user asked about #374 residue.',
			],
			stderr: 'pipe',
		});
		const stderr = result.stderr.toString();
		rmSync(tempDir, {recursive: true, force: true});

		expect(result.exitCode).toBe(1);
		expect(stderr).toContain('FAIL: tmux residue detector found');
		expect(stderr).toContain('missing exact command row');
		expect(stderr).toContain('--- tmux pane ---\nbad pane');
	});
});
