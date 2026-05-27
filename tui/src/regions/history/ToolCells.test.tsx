import {describe, expect, test} from 'bun:test';
import React from 'react';
import {render} from 'ink-testing-library';
import {ToolCallCell} from './ToolCallCell.js';
import {ToolResultCell} from './ToolResultCell.js';
import type {ToolCallHistoryMessage, ToolResultHistoryMessage} from './types.js';

describe('ToolCallCell snapshots', () => {
	test('running call shows name, status, and argument summary', () => {
		expect(
			render(
				<ToolCallCell
					message={toolCall({
						status: 'running',
						arguments: '{"path":"fixture.txt"}',
					})}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});

	test('successful call keeps the argument summary visible', () => {
		expect(
			render(
				<ToolCallCell
					message={toolCall({
						status: 'completed',
						arguments: '{"path":"fixture.txt"}',
					})}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});

	test('failed call shows the error without hiding the call', () => {
		expect(
			render(
				<ToolCallCell
					message={toolCall({
						status: 'failed',
						arguments: '{"path":"../secret.txt"}',
						errorMessage: 'path escapes workspace',
					})}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});
});

describe('ToolResultCell snapshots', () => {
	test('successful result shows a bounded snippet', () => {
		expect(
			render(
				<ToolResultCell
					message={toolResult({
						status: 'completed',
						text: '1: alpha\n2: beta',
					})}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});

	test('failed result emphasizes the error', () => {
		expect(
			render(
				<ToolResultCell
					message={toolResult({
						status: 'failed',
						text: 'path escapes workspace',
						errorMessage: 'path escapes workspace',
					})}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});

	test('truncated result includes a marker and can render expanded state', () => {
		const longText = [
			'1: alpha beta gamma delta epsilon zeta eta theta iota kappa',
			'2: lambda mu nu xi omicron pi rho sigma tau upsilon',
			'3: phi chi psi omega',
		].join('\n');

		expect(
			render(
				<ToolResultCell
					message={toolResult({status: 'completed', text: longText})}
					initialExpanded={false}
					maxCharacters={72}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
		expect(
			render(
				<ToolResultCell
					message={toolResult({status: 'completed', text: longText})}
					initialExpanded
					maxCharacters={72}
				/>,
			).lastFrame(),
		).toMatchSnapshot();
	});

	test('interactive truncated result toggles between collapsed and expanded', async () => {
		const longText = [
			'1: alpha beta gamma delta epsilon zeta eta theta iota kappa',
			'2: lambda mu nu xi omicron pi rho sigma tau upsilon',
			'3: phi chi psi omega',
		].join('\n');
		const view = render(
			<ToolResultCell
				message={toolResult({status: 'completed', text: longText})}
				interactive
				maxCharacters={72}
			/>,
		);

		expect(view.lastFrame()).toContain('truncated');
		view.stdin.write('e');
		await settle();
		expect(view.lastFrame()).toContain('expanded result');
		expect(view.lastFrame()).toContain('3: phi chi psi omega');
	});
});

function toolCall(
	overrides: Partial<ToolCallHistoryMessage> = {},
): ToolCallHistoryMessage {
	return {
		id: 'tool-call-1',
		role: 'tool_call',
		runId: 'run-1',
		toolCallId: 'tool-call-1',
		name: 'read',
		arguments: '',
		status: 'running',
		...overrides,
	};
}

function toolResult(
	overrides: Partial<ToolResultHistoryMessage> = {},
): ToolResultHistoryMessage {
	return {
		id: 'tool-result-1',
		role: 'tool_result',
		runId: 'run-1',
		toolCallId: 'tool-call-1',
		name: 'read',
		status: 'completed',
		text: '',
		...overrides,
	};
}

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 0));
}
