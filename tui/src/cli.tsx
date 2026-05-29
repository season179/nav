#!/usr/bin/env bun
import React from 'react';
import {render, type Instance, type RenderOptions} from 'ink';
import {App} from './app/App.js';
import {AlternateScreen} from './ink-ext/AlternateScreen.js';
import {
	createStdinProxy,
	MouseEventProvider,
	type StdinProxy,
} from './ink-ext/mouse.js';

type RenderApp = (node: React.ReactNode, options: RenderOptions) => Instance;
type CreateProxy = (stdin: NodeJS.ReadableStream) => StdinProxy;

type StartCliOptions = {
	backendPath: string;
	stdin?: NodeJS.ReadableStream;
	renderApp?: RenderApp;
	createProxy?: CreateProxy;
	mouseTracking?: boolean;
};

const NAV_TUI_MOUSE = 'NAV_TUI_MOUSE';

export function resolveBackendPath(
	argv: string[] = process.argv,
	env: NodeJS.ProcessEnv = process.env,
): string {
	if (argv.includes('--backend')) {
		return argv[argv.indexOf('--backend') + 1] ?? '';
	}

	return env.NAV_BACKEND ?? '';
}

export function resolveMouseTracking(
	env: NodeJS.ProcessEnv = process.env,
): boolean {
	const value = env[NAV_TUI_MOUSE]?.trim().toLowerCase();
	return value === '1' || value === 'true' || value === 'yes' || value === 'on';
}

export function startCli({
	backendPath,
	stdin = process.stdin,
	renderApp = render,
	createProxy = createStdinProxy,
	mouseTracking = false,
}: StartCliOptions): Instance {
	const {proxy, mouseEvents, dispose} = createProxy(stdin);
	let app: Instance;
	try {
		app = renderApp(
			<AlternateScreen mouseTracking={mouseTracking}>
				<MouseEventProvider emitter={mouseEvents}>
					<App backendPath={backendPath} />
				</MouseEventProvider>
			</AlternateScreen>,
			{stdin: proxy, exitOnCtrlC: false},
		);
	} catch (error) {
		dispose();
		throw error;
	}

	void app.waitUntilExit().finally(dispose);
	return app;
}

if (import.meta.main) {
	startCli({
		backendPath: resolveBackendPath(),
		mouseTracking: resolveMouseTracking(),
	});
}
