import {describe, expect, mock, test} from 'bun:test';
import {EventEmitter} from 'node:events';
import React from 'react';
import type {Instance, RenderOptions} from 'ink';
import {startCli} from './cli.js';
import {AlternateScreen} from './ink-ext/AlternateScreen.js';
import type {StdinProxy} from './ink-ext/mouse.js';

const proxy = process.stdin;
const mouseEvents = new EventEmitter();

describe('cli runtime wiring', () => {
	test('renders the App inside fullscreen mouse tracking with App-owned Ctrl+C', () => {
		const dispose = mock(() => {});
		const renderApp = mock(
			(_node: React.ReactNode, _options: RenderOptions): Instance =>
				fakeInkInstance(),
		);
		const createProxy = mock(
			(_stdin: NodeJS.ReadableStream): StdinProxy => ({
				proxy,
				mouseEvents,
				dispose,
			}),
		);

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
				mouseTracking: true,
			},
		});
	});

	test('disposes the stdin proxy when Ink render fails', () => {
		const dispose = mock(() => {});
		const renderApp = mock((): Instance => {
			throw new Error('render failed');
		});
		const createProxy = mock(
			(_stdin: NodeJS.ReadableStream): StdinProxy => ({
				proxy,
				mouseEvents,
				dispose,
			}),
		);

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
});

function fakeInkInstance(): Instance {
	return {
		clear: mock(() => {}),
		cleanup: mock(() => {}),
		rerender: mock(() => {}),
		unmount: mock(() => {}),
		waitUntilExit: async () => {},
	};
}
