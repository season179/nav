import React from 'react';
import {Box, Text} from 'ink';
import type {HistoryMessage} from './types.js';
import {theme} from './theme.js';

type Props = {
	messages: HistoryMessage[];
};

export function HistoryPane({messages}: Props) {
	return (
		<Box flexDirection="column" flexGrow={1} paddingX={2} paddingY={0}>
			{messages.length === 0 ? (
				<Box flexDirection="column" marginTop={1}>
					<Text color={theme.claude} bold>
						nav
					</Text>
					<Text color={theme.inactive}>
						Ask a question, or type /model or /exit.
					</Text>
				</Box>
			) : (
				messages.map(message => (
					<MessageRow key={message.id} message={message} />
				))
			)}
		</Box>
	);
}

function MessageRow({message}: {message: HistoryMessage}) {
	if (message.role === 'system') {
		return (
			<Box flexDirection="column" marginBottom={1}>
				<Text color={theme.inactive} wrap="wrap">
					{message.text}
				</Text>
			</Box>
		);
	}

	if (message.role === 'user') {
		return (
			<Box
				flexDirection="column"
				marginBottom={1}
				backgroundColor={theme.userMessageBackground}
				paddingX={1}
			>
				<Text wrap="wrap" color={theme.text}>
					{message.text || ' '}
				</Text>
			</Box>
		);
	}

	return (
		<Box flexDirection="column" marginBottom={1}>
			<Text wrap="wrap" color={theme.text}>
				{message.text || ' '}
			</Text>
		</Box>
	);
}
