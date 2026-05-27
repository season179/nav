import React, {useState} from 'react';
import {Box, Text, useInput} from 'ink';
import {theme} from '../../theme/index.js';
import type {ToolResultHistoryMessage} from './types.js';

type Props = {
	message: ToolResultHistoryMessage;
	initialExpanded?: boolean;
	interactive?: boolean;
	maxCharacters?: number;
};

const DEFAULT_MAX_CHARACTERS = 240;

export function ToolResultCell({
	message,
	initialExpanded = false,
	interactive = false,
	maxCharacters = DEFAULT_MAX_CHARACTERS,
}: Props): React.JSX.Element {
	const [expanded, setExpanded] = useState(initialExpanded);
	const content = message.errorMessage || message.text || '(empty result)';
	const snippet = resultSnippet(content, maxCharacters, expanded);
	const color = message.status === 'failed' ? theme.error : theme.success;

	useInput(
		(input, key) => {
			if (!snippet.truncated) {
				return;
			}
			if (input === 'e' || key.return) {
				setExpanded(value => !value);
			}
		},
		{isActive: interactive && snippet.truncated},
	);

	return (
		<Box flexDirection="column" marginBottom={1}>
			<Box>
				<Text color={color}>tool {message.name || 'tool'} result</Text>
				<Text color={theme.inactive}> {message.status}</Text>
			</Box>
			<Text color={theme.text} wrap="wrap">
				{snippet.text}
			</Text>
			{snippet.truncated ? (
				<Text color={theme.inactive}>
					{expanded
						? 'expanded result'
						: `... truncated, ${snippet.hiddenCharacters} chars hidden`}
				</Text>
			) : null}
		</Box>
	);
}

function resultSnippet(
	content: string,
	maxCharacters: number,
	expanded: boolean,
): {text: string; truncated: boolean; hiddenCharacters: number} {
	if (content.length <= maxCharacters) {
		return {text: content, truncated: false, hiddenCharacters: 0};
	}

	if (expanded) {
		return {
			text: content,
			truncated: true,
			hiddenCharacters: 0,
		};
	}

	const visibleCharacters = Math.max(0, maxCharacters - 3);
	return {
		text: `${content.slice(0, visibleCharacters)}...`,
		truncated: true,
		hiddenCharacters: content.length - visibleCharacters,
	};
}
