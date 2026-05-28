import {readFileSync} from 'node:fs';
import {Writable} from 'node:stream';
import {describe, expect, test} from 'bun:test';
import React from 'react';
import {Text, render as inkRender} from 'ink';
import {render} from 'ink-testing-library';
import {AlternateScreen} from './AlternateScreen.js';

const ENTER_ALTERNATE_SCREEN = '\x1b[?1049h\x1b[2J\x1b[H';
const ENABLE_MOUSE_TRACKING = '\x1b[?1000h\x1b[?1006h';
const EXIT_ALTERNATE_SCREEN = '\x1b[?1000l\x1b[?1006l\x1b[?1049l\x1b[?25h';
const HIDE_CURSOR = '\x1b[?25l';

describe('AlternateScreen', () => {
	test('declares signal-exit as a direct dependency', () => {
		const packageJson = JSON.parse(
			readFileSync(new URL('../../package.json', import.meta.url), 'utf8'),
		) as {dependencies: Record<string, string>};

		expect(packageJson.dependencies['signal-exit']).toBeString();
	});

	test('uses signal-exit without installing an uncaughtException handler', () => {
		const source = readFileSync(
			new URL('./AlternateScreen.tsx', import.meta.url),
			'utf8',
		);

		expect(source).toContain('signal-exit');
		expect(source).not.toContain('uncaughtException');
	});

	test('enters alternate screen before the first frame and restores on unmount', () => {
		const view = render(
			<AlternateScreen>
				<Text>first frame</Text>
			</AlternateScreen>,
		);

		expect(view.stdout.frames[0]).toBe(ENTER_ALTERNATE_SCREEN);
		expect(view.stdout.frames.join('')).toContain('first frame');
		expect(view.stdout.frames.join('')).not.toContain(HIDE_CURSOR);

		view.unmount();

		expect(view.stdout.frames.at(-1)).toBe(EXIT_ALTERNATE_SCREEN);
		view.cleanup();
	});

	test('enables SGR mouse tracking when requested', () => {
		const view = render(
			<AlternateScreen mouseTracking>
				<Text>mouse frame</Text>
			</AlternateScreen>,
		);

		expect(view.stdout.frames[0]).toBe(
			ENTER_ALTERNATE_SCREEN + ENABLE_MOUSE_TRACKING,
		);

		view.unmount();

		expect(view.stdout.frames.at(-1)).toBe(EXIT_ALTERNATE_SCREEN);
		view.cleanup();
	});

	test('emits a single restore sequence when cleanup runs more than once', () => {
		const view = render(
			<AlternateScreen>
				<Text>cleanup frame</Text>
			</AlternateScreen>,
		);

		view.unmount();
		view.cleanup();

		expect(
			view.stdout.frames.filter(frame => frame === EXIT_ALTERNATE_SCREEN),
		).toHaveLength(1);
	});

	test('constrains children to the terminal rows and columns', () => {
		const stdout = new CapturingStdout({columns: 12, rows: 4});
		const stderr = new CapturingStdout({columns: 12, rows: 4});
		const view = inkRender(
			<AlternateScreen>
				<Text>{'01234567890123456789'}</Text>
			</AlternateScreen>,
			{
				stdout: stdout as unknown as NodeJS.WriteStream,
				stderr: stderr as unknown as NodeJS.WriteStream,
				stdin: process.stdin,
				debug: true,
				exitOnCtrlC: false,
				patchConsole: false,
			},
		);
		const frame = stdout.frames.find(frame => frame.includes('012345678901'));
		if (!frame) {
			throw new Error('Expected AlternateScreen to render child text');
		}
		const lines = frame.split('\n');

		expect(lines).toHaveLength(4);
		expect(lines.every(line => line.length <= 12)).toBe(true);

		view.unmount();
		view.cleanup();
	});

	test('updates child constraints when the terminal resizes', async () => {
		const stdout = new CapturingStdout({columns: 12, rows: 4});
		const stderr = new CapturingStdout({columns: 12, rows: 4});
		const view = inkRender(
			<AlternateScreen>
				<Text>{'01234567890123456789'}</Text>
			</AlternateScreen>,
			{
				stdout: stdout as unknown as NodeJS.WriteStream,
				stderr: stderr as unknown as NodeJS.WriteStream,
				stdin: process.stdin,
				debug: true,
				exitOnCtrlC: false,
				patchConsole: false,
			},
		);

		stdout.columns = 8;
		stdout.rows = 3;
		stdout.emit('resize');
		await new Promise(resolve => setTimeout(resolve, 0));

		const frame = stdout.frames.at(-1) ?? '';
		const lines = frame.split('\n');

		expect(lines).toHaveLength(3);
		expect(lines.every(line => line.length <= 8)).toBe(true);

		view.unmount();
		view.cleanup();
	});

	test('does not repaint just to register the resize listener', async () => {
		const stdout = new CapturingStdout({columns: 12, rows: 4});
		const stderr = new CapturingStdout({columns: 12, rows: 4});
		const view = inkRender(
			<AlternateScreen>
				<Text>steady frame</Text>
			</AlternateScreen>,
			{
				stdout: stdout as unknown as NodeJS.WriteStream,
				stderr: stderr as unknown as NodeJS.WriteStream,
				stdin: process.stdin,
				debug: true,
				exitOnCtrlC: false,
				patchConsole: false,
			},
		);

		await new Promise(resolve => setTimeout(resolve, 0));

		const renderedFrames = stdout.frames.filter(frame =>
			frame.includes('steady frame'),
		);
		expect(renderedFrames).toHaveLength(1);

		view.unmount();
		view.cleanup();
	});

	test('restores the terminal on normal process exit', () => {
		const output = runExitScenario("process.emit('exit', 0);");

		expect(output).toContain(ENTER_ALTERNATE_SCREEN);
		expect(output).toContain(EXIT_ALTERNATE_SCREEN);
	});

	test('restores the terminal on SIGINT and SIGTERM', () => {
		for (const signal of ['SIGINT', 'SIGTERM']) {
			const output = runExitScenario(`process.emit('${signal}');`, signal);

			expect(output).toContain(ENTER_ALTERNATE_SCREEN);
			expect(output).toContain(EXIT_ALTERNATE_SCREEN);
		}
	});

	test('restores the terminal when rendering fails after mount', () => {
		function Boom(): React.ReactNode {
			throw new Error('boom');
		}

		const view = render(
			<AlternateScreen>
				<Text>stable frame</Text>
			</AlternateScreen>,
		);

		view.rerender(
			<AlternateScreen>
				<Boom />
			</AlternateScreen>,
		);

		const restoreIndex = view.stdout.frames.indexOf(EXIT_ALTERNATE_SCREEN);
		const errorIndex = view.stdout.frames.findIndex(
			frame => frame.includes('ERROR') && frame.includes('boom'),
		);

		expect(restoreIndex).toBeGreaterThan(-1);
		expect(errorIndex).toBeGreaterThan(restoreIndex);
		view.cleanup();
	});
});

function runExitScenario(exitStatement: string, expectedSignal?: string): string {
	const decoder = new TextDecoder();
	const script = `
		import React from 'react';
		import {Text, render} from 'ink';
		import {AlternateScreen} from './src/ink-ext/AlternateScreen.tsx';

		render(
			React.createElement(
				AlternateScreen,
				{mouseTracking: true},
				React.createElement(Text, null, 'exit frame'),
			),
			{
				stdout: process.stdout,
				stderr: process.stderr,
				stdin: process.stdin,
				debug: true,
				exitOnCtrlC: false,
				patchConsole: false,
			},
		);

		${exitStatement}
	`;
	const result = Bun.spawnSync({
		cmd: [process.execPath, '--eval', script],
		cwd: new URL('../../', import.meta.url).pathname,
		stdout: 'pipe',
		stderr: 'pipe',
	});

	if (expectedSignal) {
		expect(result.signalCode).toBe(expectedSignal);
	} else {
		expect(result.exitCode).toBe(0);
	}

	expect(decoder.decode(result.stderr)).toBe('');
	return decoder.decode(result.stdout);
}

class CapturingStdout extends Writable {
	readonly frames: string[] = [];
	columns: number;
	rows: number;
	readonly isTTY = true;

	constructor(size: {readonly columns: number; readonly rows: number}) {
		super();
		this.columns = size.columns;
		this.rows = size.rows;
	}

	override _write(
		chunk: Buffer,
		_encoding: BufferEncoding,
		callback: (error?: Error | null) => void,
	): void {
		this.frames.push(chunk.toString());
		callback();
	}
}
