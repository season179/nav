import {describe, expect, mock, test} from 'bun:test';
import {applyEventToHistory} from './App.js';
import type {NavEvent} from '../backend/client.js';
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
