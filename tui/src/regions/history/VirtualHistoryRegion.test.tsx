import {describe, expect, test} from 'bun:test';
import React from 'react';
import {render} from 'ink-testing-library';
import {VirtualHistoryRegion} from './VirtualHistoryRegion.js';
import type {HistoryMessage} from './types.js';

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
});

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
