import {describe, expect, mock, test} from 'bun:test';
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
			}),
			ASSISTANT_ID,
			warn,
		);
		expect(messages.at(-1)).toMatchObject({
			role: 'system',
			text: 'Changed file: src/app.ts',
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
});

describe('App confirmation overlay', () => {
	test('approve key sends tool.approve with the pending approval id', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('a');
		await settle();
		expect(client.approveTool).toHaveBeenCalledWith('approval-1');

		view.unmount();
	});

	test('reject key sends tool.reject with the pending approval id', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('r');
		await settle();
		expect(client.rejectTool).toHaveBeenCalledWith('approval-1');

		view.unmount();
	});

	test('escape treats the pending confirmation as rejected', async () => {
		const client = fakeApprovalClient();
		const view = render(<App backendClient={client} />);

		await openConfirmationOverlay(view);

		view.stdin.write('\x1B');
		await settle();
		expect(client.rejectTool).toHaveBeenCalledWith('approval-1');

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

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 10));
}
