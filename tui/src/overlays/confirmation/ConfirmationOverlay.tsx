import React from 'react';
import {Box, Text, useInput} from 'ink';
import {theme} from '../../theme/index.js';

export type ToolApprovalRequest = {
	approvalId: string;
	toolCallId: string;
	toolName: string;
	reason: string;
	argumentsSummary: string;
	riskClass?: string;
};

type Props = {
	request: ToolApprovalRequest;
	onApprove: () => void;
	onReject: () => void;
};

const OVERLAY_WIDTH = 56;

export function ConfirmationOverlay({
	request,
	onApprove,
	onReject,
}: Props): React.JSX.Element {
	useInput((input, key) => {
		if (key.escape) {
			onReject();
			return;
		}

		if (key.return || input.toLowerCase() === 'a') {
			onApprove();
			return;
		}

		if (input.toLowerCase() === 'r') {
			onReject();
		}
	});

	return (
		<Box flexDirection="column" paddingX={2} paddingY={1}>
			<Box marginBottom={1}>
				<Text color={theme.promptBorder}>
					{'─'.repeat(OVERLAY_WIDTH)}
				</Text>
			</Box>
			<Text bold color={theme.text}>
				Confirm tool request
			</Text>
			<Text color={theme.text}>Tool: {request.toolName || 'tool'}</Text>
			{request.riskClass ? (
				<Text color={theme.inactive}>Risk: {request.riskClass}</Text>
			) : null}
			<Box marginTop={1} flexDirection="column">
				<Text color={theme.inactive}>Reason</Text>
				<Text color={theme.text} wrap="wrap">
					{request.reason || 'The backend requested confirmation.'}
				</Text>
			</Box>
			{request.argumentsSummary ? (
				<Box marginTop={1} flexDirection="column">
					<Text color={theme.inactive}>Arguments</Text>
					<Text color={theme.text} wrap="wrap">
						{formatArguments(request)}
					</Text>
				</Box>
			) : null}
			<Box marginTop={1}>
				<Text color={theme.inactive}>
					A approve · R reject · Enter approve · Esc reject
				</Text>
			</Box>
		</Box>
	);
}

function formatArguments(request: ToolApprovalRequest): string {
	const parsed = parseJson(request.argumentsSummary);
	if (request.toolName === 'bash' && isRecord(parsed)) {
		const command = parsed.cmd ?? parsed.command;
		if (typeof command === 'string') {
			return `command: ${command}`;
		}
	}

	if (isRecord(parsed)) {
		const entries = Object.entries(parsed);
		if (entries.length > 0) {
			return entries
				.map(([key, value]) => `${key}: ${formatArgumentValue(value)}`)
				.join(', ');
		}
	}

	return request.argumentsSummary.trim();
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
