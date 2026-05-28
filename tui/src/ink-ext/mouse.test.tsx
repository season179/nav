import {describe, expect, test} from 'bun:test';
import {EventEmitter} from 'node:events';
import {PassThrough, Writable} from 'node:stream';
import React from 'react';
import {render as renderInk, Text, useInput} from 'ink';
import {render as renderTest} from 'ink-testing-library';
import {
	createStdinProxy,
	MouseEventProvider,
	useMouseEvents,
} from './mouse.js';
import type {ButtonMouseEvent, WheelMouseEvent} from './mouse.js';

describe('createStdinProxy', () => {
	test('passes typing through to the readable proxy', async () => {
		const {stdin, dispose, output} = createProxyHarness();
		stdin.write('hello');
		await settle();

		expect(output()).toBe('hello');

		dispose();
	});

	test('emits wheel events without leaking SGR bytes to the proxy', async () => {
		const {stdin, wheelEvents, dispose, output} = createProxyHarness();

		stdin.write('\x1B[<64;10;20M\x1B[<65;10;20M');
		await settle();

		expect(wheelEvents).toEqual([
			wheelEvent('up'),
			wheelEvent('down'),
		]);
		expect(output()).toBe('');

		dispose();
	});

	test('reassembles an SGR sequence split across chunks', async () => {
		const {stdin, wheelEvents, dispose, output} = createProxyHarness();

		stdin.write('\x1B[<64;10');
		await settle();
		stdin.write(';20M');
		await settle();

		expect(wheelEvents).toEqual([
			wheelEvent('up'),
		]);
		expect(output()).toBe('');

		dispose();
	});

	test('passes Enter, bare Esc, Backspace, and Ctrl+C through verbatim', async () => {
		const {stdin, dispose, output} = createProxyHarness();

		stdin.write('\r\x7F\x03');
		stdin.write('\x1B');
		await waitForPartialFlush();

		expect(output()).toBe('\r\x7F\x03\x1B');

		dispose();
	});

	test('demuxes interleaved typing and multiple SGR sequences', async () => {
		const {stdin, wheelEvents, dispose, output} = createProxyHarness();

		stdin.write('foo\x1B[<64;1;1Mbar\x1B[<65;1;1Mbaz');
		await settle();

		expect(output()).toBe('foobarbaz');
		expect(wheelEvents).toEqual([
			wheelEvent('up'),
			wheelEvent('down'),
		]);

		dispose();
	});

	test('decodes wheel modifier bits', async () => {
		const {stdin, wheelEvents, dispose} = createProxyHarness();

		stdin.write('\x1B[<93;1;1M');
		await settle();

		expect(wheelEvents).toEqual([
			wheelEvent('down', {ctrl: true, shift: true, alt: true}),
		]);

		dispose();
	});

	test('emits non-wheel SGR mouse events without leaking bytes', async () => {
		const {stdin, mouseEvents, dispose, output} = createProxyHarness();
		const buttonEvents: unknown[] = [];
		mouseEvents.on('mouse', event => {
			buttonEvents.push(event);
		});

		stdin.write('\x1B[<0;3;4M\x1B[<0;3;4m');
		await settle();

		expect(output()).toBe('');
		expect(buttonEvents).toEqual([
			buttonEvent('press'),
			buttonEvent('release'),
		]);

		dispose();
	});

	test('passes non-SGR CSI and ESC+letter sequences untouched', async () => {
		const {stdin, wheelEvents, dispose, output} = createProxyHarness();

		stdin.write('\x1B[A\x1Bx');
		await settle();

		expect(output()).toBe('\x1B[A\x1Bx');
		expect(wheelEvents).toEqual([]);

		dispose();
	});

	test('parses SGR input fragmented one byte at a time', async () => {
		const {stdin, wheelEvents, dispose, output} = createProxyHarness();

		for (const byte of Buffer.from('\x1B[<64;10;20M')) {
			stdin.write(Buffer.from([byte]));
		}
		await settle();

		expect(wheelEvents).toEqual([
			wheelEvent('up'),
		]);
		expect(output()).toBe('');

		dispose();
	});

	test('flushes malformed partial SGR bytes instead of wedging input', async () => {
		const {stdin, dispose, output} = createProxyHarness();

		stdin.write('\x1B[<99;1');
		await waitForPartialFlush();

		expect(output()).toBe('\x1B[<99;1');

		dispose();
	});

	test('delegates TTY capabilities to the real stdin stream', () => {
		const calls: string[] = [];
		const stdin = new PassThrough() as PassThrough & {
			isTTY: boolean;
			setRawMode: (enabled: boolean) => void;
			ref: () => void;
			unref: () => void;
		};
		stdin.isTTY = true;
		stdin.setRawMode = enabled => {
			calls.push(`raw:${enabled}`);
		};
		stdin.ref = () => {
			calls.push('ref');
		};
		stdin.unref = () => {
			calls.push('unref');
		};

		const {proxy, dispose} = createStdinProxy(stdin);
		const delegatedProxy = proxy as typeof proxy & {
			isTTY?: boolean;
			setRawMode: (enabled: boolean) => unknown;
			ref: () => unknown;
			unref: () => unknown;
		};

		expect(delegatedProxy.isTTY).toBe(true);
		delegatedProxy.setRawMode(true);
		delegatedProxy.ref();
		delegatedProxy.unref();

		expect(calls).toEqual(['raw:true', 'ref', 'unref']);

		dispose();
	});

	test('keeps setEncoding on the proxy instead of mutating real stdin', async () => {
		const stdin = new PassThrough();
		let sourceSetEncodingCalled = false;
		stdin.setEncoding = () => {
			sourceSetEncodingCalled = true;
			return stdin;
		};
		const {proxy, dispose} = createStdinProxy(stdin);
		const chunks: Array<string | Buffer> = [];

		proxy.setEncoding('utf8');
		proxy.on('data', chunk => {
			chunks.push(chunk);
		});
		stdin.write(Buffer.from('hello'));
		await settle();

		expect(chunks).toEqual(['hello']);
		expect(sourceSetEncodingCalled).toBe(false);

		dispose();
	});

	test('dispose flushes pending bytes before ending the proxy stream', async () => {
		const {stdin, proxy, dispose, output} = createProxyHarness();
		const ended = new Promise<void>(resolve => {
			proxy.on('end', () => {
				resolve();
			});
		});

		stdin.write('\x1B');
		dispose();
		await ended;

		expect(output()).toBe('\x1B');
	});
});

describe('MouseEventProvider', () => {
	test('useMouseEvents returns the provided emitter', () => {
		const emitter = new EventEmitter();
		const view = renderTest(
			<MouseEventProvider emitter={emitter}>
				<MouseProbe expected={emitter} />
			</MouseEventProvider>,
		);

		expect(view.lastFrame()).toBe('same');

		view.unmount();
	});
});

describe('Ink integration', () => {
	test('real Ink receives typing through the proxy stdin', async () => {
		const inputs: string[] = [];
		const harness = createInkHarness(input => {
			inputs.push(input);
		});

		await settle();
		harness.stdin.write('abc');

		await waitForExpectation(() => {
			expect(inputs.join('')).toBe('abc');
		});

		harness.cleanup();
	});

	test('real Ink does not receive wheel SGR bytes as input', async () => {
		const inputs: string[] = [];
		const wheels: unknown[] = [];
		const harness = createInkHarness(input => {
			inputs.push(input);
		});
		harness.mouseEvents.on('wheel', event => {
			wheels.push(event);
		});

		await settle();
		harness.stdin.write('\x1B[<64;1;1M');
		await settle();

		expect(inputs).toEqual([]);
		expect(wheels).toEqual([
			wheelEvent('up'),
		]);

		harness.cleanup();
	});

	test('real Ink receives interleaved typing while wheel events stay on the emitter', async () => {
		const inputs: string[] = [];
		const wheels: unknown[] = [];
		const harness = createInkHarness(input => {
			inputs.push(input);
		});
		harness.mouseEvents.on('wheel', event => {
			wheels.push(event);
		});

		await settle();
		harness.stdin.write('foo\x1B[<65;1;1Mbar');

		await waitForExpectation(() => {
			expect(inputs.join('')).toBe('foobar');
		});
		expect(wheels).toEqual([
			wheelEvent('down'),
		]);

		harness.cleanup();
	});
});

function MouseProbe({expected}: {expected: EventEmitter}): React.JSX.Element {
	const actual = useMouseEvents();
	return <Text>{actual === expected ? 'same' : 'different'}</Text>;
}

function wheelEvent(
	direction: WheelMouseEvent['direction'],
	overrides: Partial<Pick<WheelMouseEvent, 'ctrl' | 'shift' | 'alt'>> = {},
): WheelMouseEvent {
	return {
		type: 'wheel',
		direction,
		ctrl: false,
		shift: false,
		alt: false,
		...overrides,
	};
}

function buttonEvent(action: ButtonMouseEvent['action']): ButtonMouseEvent {
	return {
		type: 'mouse',
		action,
		button: 0,
		ctrl: false,
		shift: false,
		alt: false,
	};
}

function InkInputProbe({
	onInput,
}: {
	onInput: (input: string) => void;
}): React.JSX.Element {
	useInput(input => {
		if (input) {
			onInput(input);
		}
	});

	return <Text>probe</Text>;
}

function createProxyHarness(): {
	stdin: PassThrough;
	proxy: ReturnType<typeof createStdinProxy>['proxy'];
	mouseEvents: EventEmitter;
	wheelEvents: unknown[];
	dispose: () => void;
	output: () => string;
} {
	const stdin = new PassThrough();
	const {proxy, mouseEvents, dispose} = createStdinProxy(stdin);
	const chunks: Array<string | Buffer> = [];
	const wheelEvents: unknown[] = [];

	proxy.on('data', chunk => {
		chunks.push(chunk);
	});
	mouseEvents.on('wheel', event => {
		wheelEvents.push(event);
	});

	return {
		stdin,
		proxy,
		mouseEvents,
		wheelEvents,
		dispose,
		output() {
			return chunks.map(chunk => chunk.toString()).join('');
		},
	};
}

function createInkHarness(onInput: (input: string) => void): {
	stdin: PassThrough;
	mouseEvents: EventEmitter;
	cleanup: () => void;
} {
	const stdin = new PassThrough() as PassThrough & {
		isTTY: boolean;
		setRawMode: (enabled: boolean) => void;
		ref: () => void;
		unref: () => void;
	};
	stdin.isTTY = true;
	stdin.setRawMode = () => {};
	stdin.ref = () => {};
	stdin.unref = () => {};
	const {proxy, mouseEvents, dispose} = createStdinProxy(stdin);
	const stdout = new CaptureStream();
	const stderr = new CaptureStream();
	const instance = renderInk(<InkInputProbe onInput={onInput} />, {
		stdin: proxy,
		stdout: stdout as NodeJS.WriteStream,
		stderr: stderr as NodeJS.WriteStream,
		exitOnCtrlC: false,
		patchConsole: false,
	});

	return {
		stdin,
		mouseEvents,
		cleanup() {
			instance.unmount();
			dispose();
		},
	};
}

class CaptureStream extends Writable {
	columns = 80;
	rows = 24;
	isTTY = true;

	_write(
		_chunk: Buffer,
		_encoding: BufferEncoding,
		callback: (error?: Error | null) => void,
	): void {
		callback();
	}
}

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 10));
}

async function waitForPartialFlush(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 35));
}

async function waitForExpectation(expectation: () => void): Promise<void> {
	const deadline = Date.now() + 250;
	let lastError: unknown;

	while (Date.now() < deadline) {
		try {
			expectation();
			return;
		} catch (error) {
			lastError = error;
			await settle();
		}
	}

	if (lastError) {
		throw lastError;
	}
	expectation();
}
