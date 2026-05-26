import React from 'react';
import {Box, Text} from 'ink';
import TextInput from 'ink-text-input';
import {theme} from './theme.js';

/** Terminal rows: top rule + input + bottom rule + hint. */
export const COMPOSER_HEIGHT = 4;

type Props = {
	value: string;
	busy: boolean;
	hint: string;
	width: number;
	focused: boolean;
	onChange: (value: string) => void;
	onSubmit: (value: string) => void;
};

function HorizontalRule({width}: {width: number}) {
	return (
		<Box height={1} flexShrink={0}>
			<Text color={theme.promptBorder}>{'─'.repeat(Math.max(1, width))}</Text>
		</Box>
	);
}

export function Composer({
	value,
	busy,
	hint,
	width,
	focused,
	onChange,
	onSubmit,
}: Props) {
	return (
		<Box
			flexDirection="column"
			height={COMPOSER_HEIGHT}
			flexShrink={0}
			width={width}
		>
			<HorizontalRule width={width} />
			<Box flexDirection="row" height={1} flexShrink={0} width={width}>
				<Text color={theme.text}>{'>'} </Text>
				<Box flexGrow={1}>
					{busy ? (
						<Text color={theme.text}>{value || ' '}</Text>
					) : (
						<TextInput
							value={value}
							onChange={onChange}
							onSubmit={onSubmit}
							focus={focused}
							showCursor
						/>
					)}
				</Box>
			</Box>
			<HorizontalRule width={width} />
			<Box height={1} flexShrink={0}>
				<Text color={theme.inactive}>{busy ? 'Running…' : hint}</Text>
			</Box>
		</Box>
	);
}
