import {describe, expect, test} from 'bun:test';
import {readFileSync} from 'node:fs';
import path from 'node:path';
import React from 'react';
import {render} from 'ink-testing-library';
import {applyEventToHistory} from '../../app/App.js';
import {parseSse, type NavEvent} from '../../backend/client.js';
import {HistoryRegion} from './HistoryRegion.js';
import type {HistoryMessage} from './types.js';

describe('HistoryRegion text rendering', () => {
	test('keeps user, assistant, and system messages visible', () => {
		const messages: HistoryMessage[] = [
			{id: 'user-1', role: 'user', text: 'hello'},
			{id: 'assistant-1', role: 'assistant', text: 'hi there'},
			{id: 'system-1', role: 'system', text: 'Model set to openai/gpt-4.1'},
		];

		const {lastFrame} = render(
			<HistoryRegion messages={messages} height={8} />,
		);
		const frame = lastFrame() ?? '';

		expect(frame).toContain('hello');
		expect(frame).toContain('hi there');
		expect(frame).toContain('Model set to openai/gpt-4.1');
	});
});

describe('HistoryRegion tool rendering', () => {
	test('renders a recorded read stream as tool call followed by assistant result', () => {
		const assistantId = 'assistant-live';
		const initialMessages: HistoryMessage[] = [
			{id: 'user-live', role: 'user', text: 'read the fixture'},
			{id: assistantId, role: 'assistant', text: ''},
		];
		const messages = applyEvents(
			initialMessages,
			protocolFixture('tool-call-read.sse'),
			assistantId,
		);
		const {lastFrame} = render(<HistoryRegion messages={messages} height={8} />);
		const frame = lastFrame() ?? '';

		expect(frame).toContain('tool read');
		expect(frame).toContain('path: fixture.txt');
		expect(frame).toContain('read complete');
		expect(frame.indexOf('tool read')).toBeLessThan(
			frame.indexOf('read complete'),
		);
	});

	test('renders a recorded failed read stream as tool call followed by tool result', () => {
		const assistantId = 'assistant-live';
		const initialMessages: HistoryMessage[] = [
			{id: 'user-live', role: 'user', text: 'read outside the workspace'},
			{id: assistantId, role: 'assistant', text: ''},
		];
		const messages = applyEvents(
			initialMessages,
			protocolFixture('tool-call-failed.sse'),
			assistantId,
		);
		const {lastFrame} = render(<HistoryRegion messages={messages} height={10} />);
		const frame = lastFrame() ?? '';

		expect(frame).toContain('tool read failed');
		expect(frame).toContain('tool read result failed');
		expect(frame).toContain('error handled');
		expect(frame.indexOf('tool read failed')).toBeLessThan(
			frame.indexOf('tool read result failed'),
		);
		expect(frame.indexOf('tool read result failed')).toBeLessThan(
			frame.indexOf('error handled'),
		);
	});

	test('preserves scrollback when a recorded read stream appends history', async () => {
		const assistantId = 'assistant-live';
		const initialMessages: HistoryMessage[] = [
			{id: 'system-1', role: 'system', text: 'Earlier context'},
			{id: 'user-1', role: 'user', text: 'First prompt'},
			{id: 'assistant-1', role: 'assistant', text: 'First answer'},
			{id: 'user-live', role: 'user', text: 'read the fixture'},
			{id: assistantId, role: 'assistant', text: ''},
		];
		const view = render(
			<HistoryRegion messages={initialMessages} height={4} />,
		);
		view.stdin.write('\u001B[A');
		await settle();
		expect(view.lastFrame()).toContain('↓ 1 hidden');

		const messages = applyEvents(
			initialMessages,
			protocolFixture('tool-call-read.sse'),
			assistantId,
		);
		view.rerender(<HistoryRegion messages={messages} height={4} />);
		await settle();
		const frame = view.lastFrame() ?? '';

		expect(frame).toContain('First answer');
		expect(frame).not.toContain('tool read');
		expect(frame).toContain('↓ 2 hidden');
	});

	test('preserves scrollback while bash output updates the current tool row', async () => {
		const assistantId = 'assistant-live';
		let messages: HistoryMessage[] = [
			{id: 'system-1', role: 'system', text: 'Earlier context'},
			{id: 'user-1', role: 'user', text: 'First prompt'},
			{id: 'assistant-1', role: 'assistant', text: 'First answer'},
			{id: 'user-live', role: 'user', text: 'run a command'},
			{id: assistantId, role: 'assistant', text: ''},
		];
		messages = applyEventToHistory(
			messages,
			navEvent('tool.call_started', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'bash',
			}),
			assistantId,
		);

		const view = render(<HistoryRegion messages={messages} height={4} />);
		view.stdin.write('\u001B[A');
		await settle();
		const before = view.lastFrame() ?? '';
		expect(before).toContain('First answer');
		expect(before).toContain('↓ 1 hidden');

		messages = applyEventToHistory(
			messages,
			navEvent('tool.output_delta', {
				runId: 'run-1',
				toolCallId: 'tool-1',
				stream: 'stdout',
				chunk: 'line 1\nline 2\n',
			}),
			assistantId,
		);
		view.rerender(<HistoryRegion messages={messages} height={4} />);
		await settle();

		expect(view.lastFrame()).toBe(before);
	});
});

function applyEvents(
	messages: HistoryMessage[],
	input: string,
	assistantId: string,
): HistoryMessage[] {
	let nextMessages = messages;
	parseSse(input, (event: NavEvent) => {
		nextMessages = applyEventToHistory(nextMessages, event, assistantId);
		return false;
	});
	return nextMessages;
}

function protocolFixture(name: string): string {
	return readFileSync(
		path.join(
			import.meta.dir,
			'..',
			'..',
			'..',
			'..',
			'fixtures',
			'protocol',
			'event-streams',
			name,
		),
		'utf8',
	);
}

function navEvent(type: string, overrides: Partial<NavEvent>): NavEvent {
	return {
		id: `${type}-id`,
		type,
		sessionId: 'session-1',
		...overrides,
	} as NavEvent;
}

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 0));
}
