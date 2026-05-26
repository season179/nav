import React from 'react';
import {Box, Text} from 'ink';
import type {HistoryMessage} from './types.js';

type Props = {
	messages: HistoryMessage[];
};

export function HistoryPane({messages}: Props) {
	return (
		<Box flexDirection="column" flexGrow={1} paddingX={1} paddingY={0}>
			{messages.length === 0 ? (
				<Text dimColor>Messages appear here after you send a prompt.</Text>
			) : (
				messages.map(message => (
					<Box key={message.id} flexDirection="column" marginBottom={1}>
						<Text bold color={message.role === 'user' ? 'green' : 'cyan'}>
							{message.role === 'user' ? 'You' : 'nav'}
						</Text>
						<Text wrap="wrap">{message.text || ' '}</Text>
					</Box>
				))
			)}
		</Box>
	);
}
