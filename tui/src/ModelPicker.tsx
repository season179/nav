import React, {useMemo, useState} from 'react';
import {Box, Text, useInput} from 'ink';
import type {ModelOption, ModelRef} from './settings.js';
import {formatModelLabel} from './settings.js';
import {theme} from './theme.js';

type Props = {
	options: ModelOption[];
	current: ModelRef | null;
	onSelect: (ref: ModelRef) => void;
	onCancel: () => void;
};

export function ModelPicker({options, current, onSelect, onCancel}: Props) {
	const currentKey = current
		? `${current.provider}\0${current.model}`
		: null;
	const initialIndex = Math.max(
		0,
		options.findIndex(
			option =>
				`${option.provider}\0${option.model}` === currentKey,
		),
	);
	const [index, setIndex] = useState(initialIndex);

	const visible = useMemo(() => {
		const maxRows = Math.min(options.length, 12);
		const start = Math.max(
			0,
			Math.min(index - Math.floor(maxRows / 2), options.length - maxRows),
		);
		return {start, maxRows};
	}, [index, options.length]);

	useInput((input, key) => {
		if (key.escape) {
			onCancel();
			return;
		}
		if (key.upArrow) {
			setIndex(previous => Math.max(0, previous - 1));
			return;
		}
		if (key.downArrow) {
			setIndex(previous => Math.min(options.length - 1, previous + 1));
			return;
		}
		if (key.return) {
			const option = options[index];
			if (option) {
				onSelect({provider: option.provider, model: option.model});
			}
		}
	});

	return (
		<Box flexDirection="column" paddingX={2} paddingY={1}>
			<Box marginBottom={1}>
				<Text color={theme.promptBorder}>{'─'.repeat(40)}</Text>
			</Box>
			<Text bold color={theme.text}>
				Select model
			</Text>
			<Text color={theme.inactive}>
				{current
					? `Current: ${formatModelLabel(current)}`
					: 'No model selected'}
			</Text>
			<Box flexDirection="column" marginY={1}>
				{options.length === 0 ? (
					<Text color={theme.error}>
						No models in settings — add providers to ~/.nav/settings.json
					</Text>
				) : (
					options
						.slice(visible.start, visible.start + visible.maxRows)
						.map((option, offset) => {
							const rowIndex = visible.start + offset;
							const selected = rowIndex === index;
							const active =
								`${option.provider}\0${option.model}` === currentKey;
							return (
								<Box key={option.label}>
									<Text
										color={
											selected ? theme.claude : theme.text
										}
										inverse={selected}
									>
										{selected ? '› ' : '  '}
										{option.label}
										{active ? ' (current)' : ''}
									</Text>
								</Box>
							);
						})
				)}
			</Box>
			<Text color={theme.inactive}>
				↑↓ move · Enter select · Esc cancel
			</Text>
		</Box>
	);
}
