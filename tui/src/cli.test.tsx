import {describe, expect, mock, test} from 'bun:test';
import {EventEmitter} from 'node:events';
import React from 'react';
import type {Instance, RenderOptions} from 'ink';
import {resolveMouseTracking, startCli} from './cli.js';
import {AlternateScreen} from './ink-ext/AlternateScreen.js';
import type {StdinProxy} from './ink-ext/mouse.js';

const proxy = process.stdin;
const mouseEvents = new EventEmitter();

type CliHarness = {
	createProxy: ReturnType<typeof mock>;
	dispose: ReturnType<typeof mock>;
	renderApp: ReturnType<typeof mock>;
};

describe('cli runtime wiring', () => {
	test('renders the App fullscreen without mouse tracking by default', () => {
		const {createProxy, renderApp} = createCliHarness();

		startCli({
			backendPath: '/tmp/nav-backend',
			stdin: process.stdin,
			renderApp,
			createProxy,
		});

		expect(createProxy).toHaveBeenCalledWith(process.stdin);
		expect(renderApp).toHaveBeenCalledTimes(1);

		const [node, options] = renderApp.mock.calls[0]!;
		expect(options).toMatchObject({
			stdin: proxy,
			exitOnCtrlC: false,
		});
		expect(node).toMatchObject({
			type: AlternateScreen,
			props: {
				mouseTracking: false,
			},
		});
	});

	test('opts into fullscreen mouse tracking for wheel scrolling', () => {
		const {createProxy, renderApp} = createCliHarness();

		startCli({
			backendPath: '/tmp/nav-backend',
			stdin: process.stdin,
			renderApp,
			createProxy,
			mouseTracking: true,
		});

		const [node] = renderApp.mock.calls[0]!;
		expect(node).toMatchObject({
			type: AlternateScreen,
			props: {
				mouseTracking: true,
			},
		});
	});

	test('disposes the stdin proxy when Ink render fails', () => {
		const renderApp = mock((): Instance => {
			throw new Error('render failed');
		});
		const {createProxy, dispose} = createCliHarness(renderApp);

		expect(() => {
			startCli({
				backendPath: '/tmp/nav-backend',
				stdin: process.stdin,
				renderApp,
				createProxy,
			});
		}).toThrow('render failed');
		expect(dispose).toHaveBeenCalled();
	});

	test('resolves mouse tracking from NAV_TUI_MOUSE', () => {
		expect(resolveMouseTracking({} as NodeJS.ProcessEnv)).toBe(false);
		expect(resolveMouseTracking({NAV_TUI_MOUSE: '0'} as NodeJS.ProcessEnv)).toBe(
			false,
		);
		expect(resolveMouseTracking({NAV_TUI_MOUSE: '1'} as NodeJS.ProcessEnv)).toBe(
			true,
		);
		expect(
			resolveMouseTracking({NAV_TUI_MOUSE: 'true'} as NodeJS.ProcessEnv),
		).toBe(true);
		expect(
			resolveMouseTracking({NAV_TUI_MOUSE: 'yes'} as NodeJS.ProcessEnv),
		).toBe(true);
		expect(resolveMouseTracking({NAV_TUI_MOUSE: 'on'} as NodeJS.ProcessEnv)).toBe(
			true,
		);
	});
});

function createCliHarness(
	renderApp = mock(
		(_node: React.ReactNode, _options: RenderOptions): Instance =>
			fakeInkInstance(),
	),
): CliHarness {
	const dispose = mock(() => {});
	const createProxy = mock(
		(_stdin: NodeJS.ReadableStream): StdinProxy => ({
			proxy,
			mouseEvents,
			dispose,
		}),
	);

	return {createProxy, dispose, renderApp};
}

function fakeInkInstance(): Instance {
	return {
		clear: mock(() => {}),
		cleanup: mock(() => {}),
		rerender: mock(() => {}),
		unmount: mock(() => {}),
		waitUntilExit: async () => {},
	};
}
