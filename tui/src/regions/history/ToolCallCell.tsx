import React from 'react';
import {Box, Text} from 'ink';
import {theme} from '../../theme/index.js';
import type {ToolCallHistoryMessage, ToolCallStatus} from './types.js';

type Props = {
	message: ToolCallHistoryMessage;
	maxOutputLines?: number;
};

type RenderedToolOutput = {
	text: string;
	hiddenLines: number;
	emptyLabel: string;
	reserveHiddenLine?: boolean;
};

const MAX_ARGUMENT_SUMMARY = 120;
const DEFAULT_MAX_OUTPUT_LINES = 20;

export function ToolCallCell({
	message,
	maxOutputLines = DEFAULT_MAX_OUTPUT_LINES,
}: Props): React.JSX.Element {
	const name = message.name || 'tool';
	const status = toolCallStatus(message.status);
	const argumentSummary = summarizeToolArguments(message.arguments);
	const output = toolOutput(message, maxOutputLines);

	return (
		<Box flexDirection="column" marginBottom={1}>
			<Box>
				<Text color={status.color}>tool {name}</Text>
				<Text color={theme.inactive}> {status.label}</Text>
			</Box>
			{argumentSummary ? (
				<Text color={theme.inactive} wrap="wrap">
					args {argumentSummary}
				</Text>
			) : null}
			{message.errorMessage ? (
				<Text color={theme.error} wrap="wrap">
					error {message.errorMessage}
				</Text>
			) : null}
			{output ? (
				<>
					<Text color={theme.inactive}>output</Text>
					<Text color={output.text ? theme.text : theme.inactive}>
						{output.text || output.emptyLabel}
					</Text>
					{output.hiddenLines > 0 || output.reserveHiddenLine ? (
						<Text color={theme.inactive}>
							{output.hiddenLines > 0
								? `…${output.hiddenLines} more lines`
								: ' '}
						</Text>
					) : null}
				</>
			) : null}
		</Box>
	);
}

function toolCallStatus(status: ToolCallStatus): {label: string; color: string} {
	switch (status) {
		case 'requested':
			return {label: 'queued', color: theme.inactive};
		case 'running':
			return {label: 'running', color: theme.accent};
		case 'completed':
			return {label: 'success', color: theme.success};
		case 'failed':
			return {label: 'failed', color: theme.error};
		case 'approval_requested':
			return {label: 'approval required', color: theme.accent};
	}
}

function summarizeToolArguments(argumentsText: string): string {
	const trimmed = argumentsText.trim();
	if (!trimmed) {
		return '';
	}

	const parsed = parseJson(trimmed);
	if (isRecord(parsed)) {
		const entries = Object.entries(parsed);
		if (entries.length > 0) {
			return truncate(
				entries
					.map(([key, value]) => `${key}: ${formatArgumentValue(value)}`)
					.join(', '),
				MAX_ARGUMENT_SUMMARY,
			);
		}
	}

	return truncate(trimmed.replace(/\s+/g, ' '), MAX_ARGUMENT_SUMMARY);
}

function parseJson(value: string): unknown {
	try {
		return JSON.parse(value) as unknown;
	} catch {
		return null;
	}
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function formatArgumentValue(value: unknown): string {
	if (typeof value === 'string') {
		return value;
	}
	if (value === null || value === undefined) {
		return String(value);
	}
	return JSON.stringify(value);
}

function truncate(value: string, maxCharacters: number): string {
	if (value.length <= maxCharacters) {
		return value;
	}
	return `${value.slice(0, maxCharacters - 3)}...`;
}

function streamingOutputWindow(
	output: string,
	maxOutputLines: number,
	options: {padVisibleLines?: boolean} = {},
): {text: string; hiddenLines: number} {
	const lines = splitOutputLines(output);
	const visibleLineCount = Math.max(1, maxOutputLines);
	const hiddenLines = Math.max(0, lines.length - visibleLineCount);
	const visibleLines = hiddenLines > 0 ? lines.slice(hiddenLines) : lines;
	if (options.padVisibleLines) {
		while (visibleLines.length < visibleLineCount) {
			visibleLines.push('');
		}
	}

	return {
		text: visibleLines.join('\n'),
		hiddenLines,
	};
}

function toolOutput(
	message: ToolCallHistoryMessage,
	maxOutputLines: number,
): RenderedToolOutput | null {
	if (message.output !== undefined) {
		return {
			text: trimFinalOutput(message.output),
			hiddenLines: 0,
			emptyLabel: '(empty output)',
		};
	}

	if (message.streamingOutput !== undefined) {
		return {
			...streamingOutputWindow(message.streamingOutput, maxOutputLines, {
				padVisibleLines: true,
			}),
			emptyLabel: '(waiting for output)',
			reserveHiddenLine: true,
		};
	}

	if (message.status === 'running' && message.name === 'bash') {
		return {
			...streamingOutputWindow('', maxOutputLines),
			emptyLabel: '(waiting for output)',
		};
	}

	return null;
}

function trimFinalOutput(output: string): string {
	if (output.endsWith('\n')) {
		return output.slice(0, -1);
	}
	return output;
}

function splitOutputLines(output: string): string[] {
	if (!output) {
		return [];
	}

	const lines = output.split('\n');
	if (lines.at(-1) === '') {
		lines.pop();
	}
	return lines;
}
