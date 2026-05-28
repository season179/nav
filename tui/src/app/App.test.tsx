import {describe, expect, mock, test} from 'bun:test';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import React from 'react';
import {render} from 'ink-testing-library';
import {App, applyEventToHistory, type AppBackendClient} from './App.js';
import type {ApprovalResult, NavEvent, SessionInfo} from '../backend/client.js';
import type {HistoryMessage} from '../regions/history/types.js';

const ASSISTANT_ID = 'assistant-1';

describe('applyEventToHistory', () => {
	test('ignores lifecycle and reasoning events', () => {
		const warn = mock(() => {});
		const messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: 'hello'},
		];

		for (const type of [
			'session.created',
			'run.started',
			'message.completed',
			'run.completed',
			'run.cancelled',
			'model.reasoning_delta',
		]) {
			expect(
				applyEventToHistory(
					messages,
					event(type, {runId: 'run-1', messageId: 'message-1'}),
					ASSISTANT_ID,
					warn,
				),
			).toBe(messages);
		}
		expect(warn).not.toHaveBeenCalled();
	});

	test('appends assistant text from model and message deltas', () => {
		const warn = mock(() => {});
		let messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: ''},
		];

		messages = applyEventToHistory(
			messages,
			event('model.text_delta', {delta: 'hello'}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('message.delta', {text: ' from nav'}),
			ASSISTANT_ID,
			warn,
		);

		expect(messages[0]).toMatchObject({
			role: 'assistant',
			text: 'hello from nav',
		});
		expect(warn).not.toHaveBeenCalled();
	});

	test('replaces assistant text for error events', () => {
		const warn = mock(() => {});

		for (const [type, message] of [
			['run.failed', 'run failed'],
			['error', 'session failed'],
			['provider.error', 'provider failed'],
		]) {
			const messages = applyEventToHistory(
				[{id: ASSISTANT_ID, role: 'assistant', text: 'pending'}],
				event(type, {runId: 'run-1', message}),
				ASSISTANT_ID,
				warn,
			);

			expect(messages[0]).toMatchObject({
				role: 'assistant',
				text: message,
			});
		}
		expect(warn).not.toHaveBeenCalled();
	});

	test('keeps late assistant text and errors after interleaved tool events', () => {
		const warn = mock(() => {});
		let messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: ''},
		];

		messages = applyEventToHistory(
			messages,
			event('tool.call_started', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('run.failed', {
				runId: 'run-1',
				message: 'provider disconnected',
			}),
			ASSISTANT_ID,
			warn,
		);

		expect(messages.map(message => message.role)).toEqual([
			'tool_call',
			'assistant',
		]);
		expect(messages.at(-1)).toMatchObject({
			role: 'assistant',
			text: 'provider disconnected',
		});
		expect(warn).not.toHaveBeenCalled();
	});

	test('dispatches text, tool, approval, and file events without falling through to text extraction', () => {
		const warn = mock(() => {});
		let messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: ''},
		];

		messages = applyEventToHistory(
			messages,
			event('tool.call_requested', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_call',
			runId: 'run-1',
			toolCallId: 'tool-1',
			name: 'read',
			status: 'requested',
			arguments: '',
		});

		messages = applyEventToHistory(
			messages,
			event('tool.call_started', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_call',
			toolCallId: 'tool-1',
			status: 'running',
		});

		messages = applyEventToHistory(
			messages,
			event('tool.call_delta', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				argumentsDelta: '{"path"',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('tool.call_completed', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
				arguments: '{"path":"fixture.txt"}',
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_call',
			toolCallId: 'tool-1',
			status: 'completed',
			arguments: '{"path":"fixture.txt"}',
		});

		messages = applyEventToHistory(
			messages,
			event('tool.call_failed', {
				runId: 'run-1',
				toolCallId: 'tool-2',
				name: 'read',
				errorMessage: 'path escapes workspace',
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_result',
			toolCallId: 'tool-2',
			name: 'read',
			status: 'failed',
			errorMessage: 'path escapes workspace',
		});

		messages = applyEventToHistory(
			messages,
			event('tool.approval_requested', {
				runId: 'run-1',
				toolCallId: 'tool-3',
				approvalId: 'approval-1',
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_call',
			toolCallId: 'tool-3',
			status: 'approval_requested',
			approvalId: 'approval-1',
		});

		messages = applyEventToHistory(
			messages,
			event('file.changed', {
				fileChangeId: 'change-1',
				path: 'src/app.ts',
				kind: 'modified',
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'file_changed',
			path: 'src/app.ts',
			kind: 'modified',
		});
		expect(warn).not.toHaveBeenCalled();
	});

	test('logs unknown events without throwing or changing history', () => {
		const warn = mock(() => {});
		const messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: 'hello'},
		];

		expect(
			applyEventToHistory(
				messages,
				event('future.event'),
				ASSISTANT_ID,
				warn,
			),
		).toBe(messages);
		expect(warn).toHaveBeenCalledWith('Unknown nav event type: future.event');
	});

	test('updates the same tool call when a completed call later fails', () => {
		const warn = mock(() => {});
		let messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: ''},
		];

		messages = applyEventToHistory(
			messages,
			event('tool.call_started', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('tool.call_completed', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
				arguments: '{"path":"../secret.txt"}',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('tool.call_failed', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'read',
				errorMessage: 'path escapes workspace',
			}),
			ASSISTANT_ID,
			warn,
		);

		const toolCalls = messages.filter(message => message.role === 'tool_call');
		expect(toolCalls).toHaveLength(1);
		expect(toolCalls[0]).toMatchObject({
			toolCallId: 'tool-1',
			status: 'failed',
			arguments: '{"path":"../secret.txt"}',
			errorMessage: 'path escapes workspace',
		});
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_result',
			toolCallId: 'tool-1',
			status: 'failed',
			errorMessage: 'path escapes workspace',
		});
		expect(warn).not.toHaveBeenCalled();
	});

	test('appends tool output deltas to the existing tool call row', () => {
		const warn = mock(() => {});
		let messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: ''},
		];

		messages = applyEventToHistory(
			messages,
			event('tool.call_started', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'bash',
			}),
			ASSISTANT_ID,
			warn,
		);
		const rowCount = messages.length;

		messages = applyEventToHistory(
			messages,
			event('tool.output_delta', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				stream: 'stdout',
				chunk: 'one\n',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('tool.output_delta', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				stream: 'stdout',
				chunk: 'two\n',
			}),
			ASSISTANT_ID,
			warn,
		);

		expect(messages).toHaveLength(rowCount);
		expect(messages.at(-1)).toMatchObject({
			role: 'tool_call',
			toolCallId: 'tool-1',
			streamingOutput: 'one\ntwo\n',
		});
		expect(warn).not.toHaveBeenCalled();
	});

	test('stores final tool output and clears the streaming buffer on completion', () => {
		const warn = mock(() => {});
		let messages: HistoryMessage[] = [
			{id: ASSISTANT_ID, role: 'assistant', text: ''},
		];

		messages = applyEventToHistory(
			messages,
			event('tool.call_started', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'bash',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('tool.output_delta', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				stream: 'stdout',
				chunk: 'live\n',
			}),
			ASSISTANT_ID,
			warn,
		);
		messages = applyEventToHistory(
			messages,
			event('tool.call_completed', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'bash',
				arguments: '{"command":"printf final"}',
				output: 'final\n',
				outputLossy: false,
			}),
			ASSISTANT_ID,
			warn,
		);

		const toolCall = messages.at(-1);
		expect(toolCall).toMatchObject({
			role: 'tool_call',
			toolCallId: 'tool-1',
			status: 'completed',
			output: 'final\n',
			outputLossy: false,
		});
		expect(
			(toolCall as {streamingOutput?: string} | undefined)?.streamingOutput,
		).toBeUndefined();
		expect(warn).not.toHaveBeenCalled();
	});
});

describe('App confirmation overlay', () => {
	test('approve key sends tool.approve with the pending approval id', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('a');
		await waitForExpectation(() => {
			expect(client.approveTool).toHaveBeenCalledWith('approval-1');
		});

		view.unmount();
	});

	test('reject key sends tool.reject with the pending approval id', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('r');
		await waitForExpectation(() => {
			expect(client.rejectTool).toHaveBeenCalledWith('approval-1');
		});

		view.unmount();
	});

	test('escape treats the pending confirmation as rejected', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('\x1B');
		await waitForExpectation(() => {
			expect(client.rejectTool).toHaveBeenCalledWith('approval-1');
		});

		view.unmount();
	});

	test('composer stays inert while the confirmation overlay is open during a busy run', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('x');
		await settle();
		expect(view.lastFrame()).toContain('Confirm tool request');
		expect(view.lastFrame()).not.toContain('> x');
		expect(client.streamMessage).toHaveBeenCalledTimes(1);

		view.unmount();
	});

	test('repeated approval keys do not send duplicate approval RPCs', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('a');
		view.stdin.write('a');
		await settle();
		expect(client.approveTool).toHaveBeenCalledTimes(1);

		view.unmount();
	});

	test('terminal run event clears a stale confirmation overlay', async () => {
		const client = fakeApprovalClient({
			terminalEvent: event('run.cancelled', {runId: 'run-1'}),
		});
		const view = render(<App backendClient={client} />);

		view.stdin.write('run command');
		await settle();
		view.stdin.write('\r');
		await settle();
		expect(view.lastFrame()).not.toContain('Confirm tool request');
		expect(client.approveTool).not.toHaveBeenCalled();
		expect(client.rejectTool).not.toHaveBeenCalled();

		view.unmount();
	});
});

describe('App fullscreen runtime controls', () => {
	test('Ctrl+C during an active stream aborts the turn and closes the backend', async () => {
		const streamStarted = deferred<void>();
		let streamSignal: AbortSignal | undefined;
		const client = fakeApprovalClient();
		client.streamMessage = mock(async function* (
			_text: string,
			options?: {signal: AbortSignal},
		) {
			streamSignal = options?.signal;
			streamStarted.resolve();
			await waitForAbort(options?.signal);
		});
		const view = render(<App backendClient={client} />);

		view.stdin.write('run command');
		await settle();
		view.stdin.write('\r');
		await streamStarted.promise;

		expect(streamSignal).toBeDefined();

		view.stdin.write('\x03');
		await waitForExpectation(() => {
			expect(streamSignal?.aborted).toBe(true);
			expect(client.close).toHaveBeenCalled();
		});

		view.unmount();
	});

	test('Ctrl+C closes the backend while the model picker is open', async () => {
		const client = fakeApprovalClient();
		const {path: settingsPath, cleanup} = writeModelSettings();
		const previousSettingsPath = process.env.NAV_MODEL_SETTINGS;
		process.env.NAV_MODEL_SETTINGS = settingsPath;

		try {
			const view = render(<App backendClient={client} />);
			view.stdin.write('/model');
			await settle();
			view.stdin.write('\r');
			await waitForExpectation(() => {
				expect(view.lastFrame()).toContain('Select model');
			});

			view.stdin.write('\x03');
			await waitForExpectation(() => {
				expect(client.close).toHaveBeenCalled();
			});

			view.unmount();
		} finally {
			if (previousSettingsPath === undefined) {
				delete process.env.NAV_MODEL_SETTINGS;
			} else {
				process.env.NAV_MODEL_SETTINGS = previousSettingsPath;
			}
			cleanup();
		}
	});
});

function event(type: string, overrides: Partial<NavEvent> = {}): NavEvent {
	if (type === 'future.event') {
		return {
			id: `${type}-id`,
			type: 'unknown',
			rawType: type,
			sessionId: 'session-1',
			payload: {},
			...overrides,
		} as NavEvent;
	}

	return {
		id: `${type}-id`,
		type,
		sessionId: 'session-1',
		...overrides,
	} as NavEvent;
}

type TestBackendClient = AppBackendClient & {
	streamMessage: ReturnType<typeof mock>;
	approveTool: ReturnType<typeof mock>;
	rejectTool: ReturnType<typeof mock>;
	reconnect: ReturnType<typeof mock>;
	close: ReturnType<typeof mock>;
};

function fakeApprovalClient({
	terminalEvent,
}: {
	terminalEvent?: NavEvent;
} = {}): TestBackendClient {
	let finishRun: () => void = () => {};

	const client = {
		streamMessage: mock(async function* (_text: string) {
			yield event('tool.approval_requested', {
				runId: 'run-1',
				toolCallId: 'tool-call-1',
				approvalId: 'approval-1',
				toolName: 'bash',
				reason: 'bash requires approval',
				argumentsSummary: '{"cmd":"echo hi"}',
				riskClass: 'exec',
			});
			if (terminalEvent) {
				yield terminalEvent;
				return;
			}
			await new Promise<void>(resolve => {
				finishRun = resolve;
			});
			yield event('run.completed', {runId: 'run-1'});
		}),
		approveTool: mock(async (_approvalId: string): Promise<ApprovalResult> => {
			finishRun();
			return {approvalId: 'approval-1', outcome: 'approved'};
		}),
		rejectTool: mock(async (_approvalId: string): Promise<ApprovalResult> => {
			finishRun();
			return {approvalId: 'approval-1', outcome: 'rejected'};
		}),
		reconnect: mock(async (): Promise<SessionInfo> => ({
			sessionId: 'session-1',
			endpoint: 'http://fake',
			cwd: '/tmp',
		})),
		close: mock(async (): Promise<void> => {}),
	};

	return client;
}

async function openConfirmationOverlay(
	view: ReturnType<typeof render>,
): Promise<void> {
	view.stdin.write('run command');
	await settle();
	view.stdin.write('\r');
	await settle();
	expect(view.lastFrame()).toContain('Confirm tool request');
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

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 10));
}

async function waitForAbort(signal?: AbortSignal): Promise<void> {
	if (!signal || signal.aborted) {
		return;
	}
	await new Promise<void>(resolve => {
		signal.addEventListener('abort', () => resolve(), {once: true});
	});
}

function deferred<T>(): {
	promise: Promise<T>;
	resolve: (value: T | PromiseLike<T>) => void;
} {
	let resolve!: (value: T | PromiseLike<T>) => void;
	const promise = new Promise<T>(nextResolve => {
		resolve = nextResolve;
	});
	return {promise, resolve};
}

function writeModelSettings(): {path: string; cleanup: () => void} {
	const directory = mkdtempSync(path.join(tmpdir(), 'nav-app-test-'));
	const filePath = path.join(directory, 'settings.json');
	writeFileSync(
		filePath,
		JSON.stringify({
			defaultModel: {provider: 'test', model: 'model-a'},
			providers: {
				test: {models: [{id: 'model-a'}]},
			},
		}),
	);
	return {
		path: filePath,
		cleanup() {
			rmSync(directory, {recursive: true, force: true});
		},
	};
}
