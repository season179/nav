import {describe, expect, test} from 'bun:test';
import {EventEmitter} from 'node:events';
import {readFileSync} from 'node:fs';
import path from 'node:path';
import React from 'react';
import {render} from 'ink-testing-library';
import {applyEventToHistory} from '../../app/App.js';
import {parseSse, type NavEvent} from '../../backend/client.js';
import {MouseEventProvider, type WheelMouseEvent} from '../../ink-ext/mouse.js';
import {VirtualHistoryRegion} from './VirtualHistoryRegion.js';
import type {HistoryMessage, ToolCallHistoryMessage} from './types.js';

describe('VirtualHistoryRegion rendering', () => {
	test('keeps user, assistant, and system messages visible', () => {
		const messages: HistoryMessage[] = [
			{id: 'user-1', role: 'user', text: 'hello'},
			{id: 'assistant-1', role: 'assistant', text: 'hi there'},
			{id: 'system-1', role: 'system', text: 'Model set to openai/gpt-4.1'},
		];

		const {lastFrame} = render(
			<VirtualHistoryRegion messages={messages} height={8} />,
		);
		const frame = lastFrame() ?? '';

		expect(frame).toContain('hello');
		expect(frame).toContain('hi there');
		expect(frame).toContain('Model set to openai/gpt-4.1');
	});

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
		const {lastFrame} = render(
			<VirtualHistoryRegion messages={messages} height={8} />,
		);
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
		const {lastFrame} = render(
			<VirtualHistoryRegion messages={messages} height={10} />,
		);
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
});

describe('VirtualHistoryRegion follow-tail streaming', () => {
	test('keeps latest streamed assistant line visible when viewport starts at bottom', async () => {
		let messages: HistoryMessage[] = [
			{id: 'user-1', role: 'user', text: 'tell me a story'},
			{id: 'assistant-1', role: 'assistant', text: 'Once upon a time', contentVersion: 1},
		];
		const {rerender, lastFrame} = render(
			<MouseEventProvider emitter={new EventEmitter()}>
				<VirtualHistoryRegion messages={messages} height={4} />
			</MouseEventProvider>,
		);
		await waitForExpectation(() => {
			expect(lastFrame()).toContain('Once upon a time');
		});

		// Grow the assistant message with more streamed content
		messages = [
			{id: 'user-1', role: 'user', text: 'tell me a story'},
			{id: 'assistant-1', role: 'assistant', text: 'Once upon a time\nin a land far away\nwhere dragons roamed\nand heroes were born', contentVersion: 2},
		];
		rerender(
			<MouseEventProvider emitter={new EventEmitter()}>
				<VirtualHistoryRegion messages={messages} height={4} />
			</MouseEventProvider>,
		);

		await waitForExpectation(() => {
			expect(lastFrame()).toContain('and heroes were born');
		});
	});

	test('keeps latest streaming tool output visible as it grows', async () => {
		let messages: HistoryMessage[] = [
			{id: 'user-1', role: 'user', text: 'run the build'},
			{
				id: 'tool-call-1',
				role: 'tool_call',
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'bash',
				arguments: JSON.stringify({command: 'bun run build'}),
				status: 'running',
				streamingOutput: 'line 1 output',
				contentVersion: 1,
			},
		];
		const {rerender, lastFrame} = render(
			<MouseEventProvider emitter={new EventEmitter()}>
				<VirtualHistoryRegion messages={messages} height={6} />
			</MouseEventProvider>,
		);
		await waitForExpectation(() => {
			expect(lastFrame()).toContain('line 1 output');
		});

		// Grow the tool output with more streaming content
		messages = [
			{id: 'user-1', role: 'user', text: 'run the build'},
			{
				id: 'tool-call-1',
				role: 'tool_call',
				runId: 'run-1',
				toolCallId: 'tool-1',
				name: 'bash',
				arguments: JSON.stringify({command: 'bun run build'}),
				status: 'running',
				streamingOutput: 'line 1 output\nline 2 output\nline 3 output\nline 4 output\nline 5 output\nline 6 output\nline 7 output',
				contentVersion: 2,
			},
		];
		rerender(
			<MouseEventProvider emitter={new EventEmitter()}>
				<VirtualHistoryRegion messages={messages} height={6} />
			</MouseEventProvider>,
		);

		await waitForExpectation(() => {
			expect(lastFrame()).toContain('line 7 output');
		});
	});

	test('scrolling up disables auto-follow so new content does not yank user to bottom', async () => {
		const emitter = new EventEmitter();
		let messages: HistoryMessage[] = Array.from({length: 12}, (_, index): HistoryMessage => ({
			id: `message-${index}`,
			role: 'assistant',
			text: `assistant line ${index}`,
			contentVersion: 1,
		}));
		const {rerender, lastFrame} = render(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={4} />
			</MouseEventProvider>,
		);
		await waitForExpectation(() => {
			expect(lastFrame()).toContain('assistant line 11');
		});

		// Scroll up to disable auto-follow
		for (let index = 0; index < 5; index += 1) {
			emitter.emit('wheel', wheelEvent('up'));
		}
		await waitForExpectation(() => {
			expect(lastFrame()).not.toContain('assistant line 11');
		});

		// Add new streamed content
		messages = [
			...messages,
			{id: 'message-12', role: 'assistant', text: 'assistant line 12', contentVersion: 1},
		];
		rerender(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={4} />
			</MouseEventProvider>,
		);
		await settle();

		// Viewport should NOT have jumped back to bottom
		expect(lastFrame()).not.toContain('assistant line 12');
	});

	test('scrolling back to bottom re-enables follow-tail for subsequent content', async () => {
		const emitter = new EventEmitter();
		let messages: HistoryMessage[] = Array.from({length: 12}, (_, index): HistoryMessage => ({
			id: `message-${index}`,
			role: 'assistant',
			text: `assistant line ${index}`,
			contentVersion: 1,
		}));
		const {rerender, lastFrame} = render(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={4} />
			</MouseEventProvider>,
		);
		await waitForExpectation(() => {
			expect(lastFrame()).toContain('assistant line 11');
		});

		// Scroll up
		for (let index = 0; index < 5; index += 1) {
			emitter.emit('wheel', wheelEvent('up'));
		}
		await waitForExpectation(() => {
			expect(lastFrame()).not.toContain('assistant line 11');
		});

		// Scroll back down to bottom
		for (let index = 0; index < 10; index += 1) {
			emitter.emit('wheel', wheelEvent('down'));
		}
		await waitForExpectation(() => {
			expect(lastFrame()).toContain('assistant line 11');
		});

		// Add new streamed content - follow-tail should be re-enabled
		messages = [
			...messages,
			{id: 'message-12', role: 'assistant', text: 'assistant line 12', contentVersion: 1},
		];
		rerender(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={4} />
			</MouseEventProvider>,
		);

		await waitForExpectation(() => {
			expect(lastFrame()).toContain('assistant line 12');
		});
	});
});

describe('VirtualHistoryRegion residue checks', () => {
	test('renders tool commands as standalone rows without residue', async () => {
		const predictedRowCount = 53;
		const commands = [
			'pwd',
			'ls tui/src',
			'rg VirtualHistoryRegion tui/src',
			'bun test',
			'bun run typecheck',
			'git status --short',
		];
		const messages = spikeResidueMessages(commands);
		const {lastFrame} = render(
			<VirtualHistoryRegion
				messages={messages}
				height={predictedRowCount}
			/>,
		);
		await waitForExpectation(() => {
			expect(lastFrame()).toContain(
				'Final assistant message: residue check complete.',
			);
		});

		const frame = lastFrame() ?? '';
		const rows = capturedRows(frame);

		for (const command of commands) {
			expect(rows).toContain(`args command: ${command}`);
		}
		expect(rows.some(row => countSubstrings(row, 'command:') > 1)).toBe(false);
		expect(
			rows.some(row => row.includes('output') && row.includes('command:')),
		).toBe(false);
		expect(rows).toHaveLength(predictedRowCount);
	});

	test('ignores keyboard paging because wheel events own history scrolling', async () => {
		const messages = Array.from({length: 12}, (_, index): HistoryMessage => ({
			id: `message-${index}`,
			role: 'assistant',
			text: `assistant line ${index}`,
		}));
		const view = render(<VirtualHistoryRegion messages={messages} height={4} />);
		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('assistant line 11');
		});
		const before = view.lastFrame();

		view.stdin.write('\u001B[A');
		view.stdin.write('\u001B[5~');
		await settle();

		expect(view.lastFrame()).toBe(before);
	});

	test('scrolls history through wheel events', async () => {
		const messages = Array.from({length: 20}, (_, index): HistoryMessage => ({
			id: `message-${index}`,
			role: 'assistant',
			text: `assistant line ${index}`,
		}));
		const {emitter, view} = renderWithMouse(
			<VirtualHistoryRegion messages={messages} height={4} />,
		);
		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('assistant line 19');
		});
		const before = view.lastFrame() ?? '';

		for (let index = 0; index < 3; index += 1) {
			emitter.emit('wheel', wheelEvent('up'));
		}

		await waitForExpectation(() => {
			expect(view.lastFrame()).not.toBe(before);
		});
		const after = view.lastFrame() ?? '';
		expect(after).toContain('hidden');
		expect(after).not.toContain('assistant line 19');
	});

	test('restores follow-tail after wheel scrolling back to the bottom', async () => {
		let messages = streamingConversation(3, 1);
		const {emitter, view} = renderWithMouse(
			<VirtualHistoryRegion messages={messages} height={5} />,
		);
		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('stream line 3');
		});

		emitter.emit('wheel', wheelEvent('up'));
		await waitForExpectation(() => {
			const frame = view.lastFrame() ?? '';
			expect(frame).toContain('hidden');
			expect(frame).not.toContain('stream line 3');
		});

		emitter.emit('wheel', wheelEvent('down'));
		await waitForExpectation(() => {
			const frame = view.lastFrame() ?? '';
			expect(frame).toContain('stream line 3');
			expect(frame).not.toContain('hidden');
		});

		messages = streamingConversation(8, 2);
		view.rerender(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={5} />
			</MouseEventProvider>,
		);

		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('stream line 8');
		});
	});

	test('does not follow new assistant output after wheel scrolling away', async () => {
		let messages = streamingConversation(3, 1);
		const {emitter, view} = renderWithMouse(
			<VirtualHistoryRegion messages={messages} height={5} />,
		);
		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('stream line 3');
		});

		emitter.emit('wheel', wheelEvent('up'));
		await waitForExpectation(() => {
			const frame = view.lastFrame() ?? '';
			expect(frame).toContain('hidden');
			expect(frame).not.toContain('stream line 3');
		});

		messages = streamingConversation(8, 2);
		view.rerender(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={5} />
			</MouseEventProvider>,
		);

		await waitForExpectation(() => {
			const frame = view.lastFrame() ?? '';
			expect(frame).toContain('hidden');
			expect(frame).not.toContain('stream line 8');
		});
	});

	test('keeps following after wheel-up when there is no hidden history', async () => {
		let messages = compactStreamingConversation(1, 1);
		const {emitter, view} = renderWithMouse(
			<VirtualHistoryRegion messages={messages} height={8} />,
		);
		await waitForExpectation(() => {
			const frame = view.lastFrame() ?? '';
			expect(frame).toContain('stream line 1');
			expect(frame).not.toContain('hidden');
		});

		emitter.emit('wheel', wheelEvent('up'));
		await settle();

		messages = compactStreamingConversation(8, 2);
		view.rerender(
			<MouseEventProvider emitter={emitter}>
				<VirtualHistoryRegion messages={messages} height={8} />
			</MouseEventProvider>,
		);

		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('stream line 8');
		});
	});

	test('keeps running tool output tail visible while following bottom', async () => {
		let messages = runningToolConversation(undefined, 1);
		const view = render(
			<VirtualHistoryRegion messages={messages} height={6} />,
		);
		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('(waiting for output)');
		});

		messages = runningToolConversation(toolOutputLines(8), 2);
		view.rerender(
			<VirtualHistoryRegion messages={messages} height={6} />,
		);

		await waitForExpectation(() => {
			expect(view.lastFrame()).toContain('tool output line 8');
		});
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

function spikeResidueMessages(commands: string[]): HistoryMessage[] {
	const messages: HistoryMessage[] = [];

	for (const [index, command] of commands.entries()) {
		const number = index + 1;
		const runId = `run-${number}`;
		const toolCallId = `tool-${number}`;
		messages.push(
			{
				id: `tool-call-${number}`,
				role: 'tool_call',
				runId,
				toolCallId,
				name: 'bash',
				arguments: JSON.stringify({command}),
				status: 'completed',
				output: `line ${number}\n`,
			},
			{
				id: `tool-result-${number}`,
				role: 'tool_result',
				runId,
				toolCallId,
				name: 'bash',
				status: 'completed',
				text: `command ${number} completed`,
			},
		);
	}

	messages.push({
		id: 'assistant-final',
		role: 'assistant',
		text: 'Final assistant message: residue check complete.',
	});

	return messages;
}

function streamingConversation(
	tailLineCount: number,
	contentVersion: number,
): HistoryMessage[] {
	const messages = Array.from({length: 8}, (_value, index): HistoryMessage => ({
		id: `context-${index + 1}`,
		role: 'assistant',
		text: `context line ${index + 1}`,
	}));

	messages.push({
		id: 'assistant-streaming',
		role: 'assistant',
		text: numberedLines('stream line', tailLineCount),
		contentVersion,
	});

	return messages;
}

function compactStreamingConversation(
	tailLineCount: number,
	contentVersion: number,
): HistoryMessage[] {
	return [
		{id: 'context-1', role: 'assistant', text: 'context line 1'},
		{
			id: 'assistant-streaming',
			role: 'assistant',
			text: numberedLines('stream line', tailLineCount),
			contentVersion,
		},
	];
}

function runningToolConversation(
	streamingOutput: string | undefined,
	contentVersion: number,
): HistoryMessage[] {
	const toolCall: ToolCallHistoryMessage = {
		id: 'tool-call-streaming',
		role: 'tool_call',
		runId: 'run-streaming',
		toolCallId: 'tool-streaming',
		name: 'bash',
		arguments: '{"command":"seq 8"}',
		status: 'running',
		contentVersion,
	};

	if (streamingOutput !== undefined) {
		toolCall.streamingOutput = streamingOutput;
	}

	return [
		{id: 'context-1', role: 'assistant', text: 'context line 1'},
		{id: 'context-2', role: 'assistant', text: 'context line 2'},
		toolCall,
	];
}

function toolOutputLines(lineCount: number): string {
	return numberedLines('tool output line', lineCount);
}

function numberedLines(label: string, lineCount: number): string {
	return Array.from(
		{length: lineCount},
		(_value, index) => `${label} ${index + 1}`,
	).join('\n');
}

function capturedRows(frame: string): string[] {
	return frame.split('\n').map(row => row.trim());
}

function countSubstrings(value: string, search: string): number {
	return value.split(search).length - 1;
}

async function waitForExpectation(assertion: () => void): Promise<void> {
	const deadline = Date.now() + 500;
	let lastError: unknown;
	while (Date.now() < deadline) {
		try {
			assertion();
			return;
		} catch (error) {
			lastError = error;
			await settle();
		}
	}

	if (lastError) {
		throw lastError;
	}
}

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 0));
}

function renderWithMouse(node: React.ReactElement): {
	emitter: EventEmitter;
	view: ReturnType<typeof render>;
} {
	const emitter = new EventEmitter();
	const view = render(
		<MouseEventProvider emitter={emitter}>{node}</MouseEventProvider>,
	);
	return {emitter, view};
}

function wheelEvent(direction: WheelMouseEvent['direction']): WheelMouseEvent {
	return {
		type: 'wheel',
		direction,
		ctrl: false,
		shift: false,
		alt: false,
	};
}
