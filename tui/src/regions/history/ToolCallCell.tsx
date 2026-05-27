import React from 'react';
import {Box, Text} from 'ink';
import {theme} from '../../theme/index.js';
import type {ToolCallHistoryMessage, ToolCallStatus} from './types.js';

type Props = {
	message: ToolCallHistoryMessage;
};

const MAX_ARGUMENT_SUMMARY = 120;

export function ToolCallCell({message}: Props): React.JSX.Element {
	const name = message.name || 'tool';
	const status = toolCallStatus(message.status);
	const argumentSummary = summarizeToolArguments(message.arguments);

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
