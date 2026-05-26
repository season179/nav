import React from 'react';
import {Box, Text} from 'ink';
import TextInput from 'ink-text-input';

/** Terminal rows reserved for composer (top border + input + hint + slack). */
export const COMPOSER_HEIGHT = 4;

type Props = {
	value: string;
	busy: boolean;
	hint: string;
	width: number;
	onChange: (value: string) => void;
	onSubmit: (value: string) => void;
};

export function Composer({
	value,
	busy,
	hint,
	width,
	onChange,
	onSubmit,
}: Props) {
	return (
		<Box
			flexDirection="column"
			height={COMPOSER_HEIGHT}
			flexShrink={0}
			width={width}
			borderStyle="single"
			borderTop
			borderColor="gray"
			paddingX={1}
		>
			<Box height={1} flexShrink={0}>
				<Text color="green">{'> '}</Text>
				{busy ? (
					<Text dimColor>{value || ' '}</Text>
				) : (
					<TextInput
						value={value}
						onChange={onChange}
						onSubmit={onSubmit}
						focus
						showCursor
					/>
				)}
			</Box>
			<Box height={1} flexShrink={0}>
				<Text dimColor>{busy ? 'Running…' : hint}</Text>
			</Box>
		</Box>
	);
}
