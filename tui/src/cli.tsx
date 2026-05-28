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
};

export function resolveBackendPath(
	argv: string[] = process.argv,
	env: NodeJS.ProcessEnv = process.env,
): string {
	if (argv.includes('--backend')) {
		return argv[argv.indexOf('--backend') + 1] ?? '';
	}

	return env.NAV_BACKEND ?? '';
}

export function startCli({
	backendPath,
	stdin = process.stdin,
	renderApp = render,
	createProxy = createStdinProxy,
}: StartCliOptions): Instance {
	const {proxy, mouseEvents, dispose} = createProxy(stdin);
	let app: Instance;
	try {
		app = renderApp(
			<AlternateScreen mouseTracking>
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
	startCli({backendPath: resolveBackendPath()});
}
