import React, {useState, useEffect, useRef} from 'react';
import {Box, Text, useInput} from 'ink';
import {ToolCallCell} from './ToolCallCell.js';
import {ToolResultCell} from './ToolResultCell.js';
import type {HistoryMessage} from './types.js';
import {theme} from '../../theme/index.js';
import {Markdown} from '../../markdown/Markdown.js';

type Props = {
	messages: HistoryMessage[];
	/** Available terminal rows for the history viewport. */
	height: number;
};

const SCROLL_STEP = 5;

/**
 * Maximum scrollback position. When scrolled, the indicator takes one
 * row, leaving `height - 1` rows for visible messages. The max number
 * of hidden messages is `messages.length - (height - 1)`, which is the
 * most we can hide before running out of visible space.
 */
function scrollbackLimit(messageCount: number, viewportHeight: number): number {
	return Math.max(0, messageCount - viewportHeight);
}

/**
 * Scrollable message history.
 * Parent clips to fixed height via overflow:hidden — this component
 * relies on that clipping rather than managing its own height.
 */
export function HistoryRegion({messages, height}: Props) {
	const [scrollback, setScrollback] = useState(0);
	const scrollbackRef = useRef(0);
	const prevCountRef = useRef(messages.length);

	useEffect(() => {
		if (messages.length === 0) {
			setScrollback(0);
			scrollbackRef.current = 0;
			prevCountRef.current = 0;
			return;
		}

		const delta = messages.length - prevCountRef.current;
		prevCountRef.current = messages.length;

		if (delta > 0 && scrollbackRef.current > 0) {
			scrollbackRef.current += delta;
			setScrollback(scrollbackRef.current);
		}

		// Clamp if messages decreased or viewport shrank
		const cap = scrollbackLimit(messages.length, height);
		if (scrollbackRef.current > cap) {
			scrollbackRef.current = cap;
			setScrollback(cap);
		}
	}, [messages.length, height]);

	useInput((_character, key) => {
		const up = key.pageUp || key.upArrow;
		const down = key.pageDown || key.downArrow;
		if (!up && !down) return;

		const step = key.pageUp || key.pageDown ? SCROLL_STEP : 1;
		const cap = scrollbackLimit(messages.length, height);
		if (up) {
			const next = Math.min(scrollbackRef.current + step, cap);
			scrollbackRef.current = next;
			setScrollback(next);
		} else {
			const next = Math.max(scrollbackRef.current - step, 0);
			scrollbackRef.current = next;
			setScrollback(next);
		}
	});

	const indicatorVisible = scrollback > 0;
	const end = messages.length - scrollback;
	const visibleMessages = messages.slice(0, Math.max(0, end));

	return (
		<Box
			flexDirection="column"
			flexGrow={1}
			paddingX={2}
			paddingY={0}
			justifyContent="flex-end"
		>
			{messages.length === 0 ? (
				<Box flexDirection="column" marginTop={1}>
					<Text color={theme.accent} bold>
						nav
					</Text>
					<Text color={theme.inactive}>
						Ask a question, or type /model or /exit.
					</Text>
				</Box>
			) : (
				<>
					{visibleMessages.map(message => (
						<MessageRow key={message.id} message={message} />
					))}
					{indicatorVisible && (
						<Box justifyContent="center">
							<Text color={theme.inactive}>
								↓ {scrollback} hidden · PgDn reveal · PgUp older
							</Text>
						</Box>
					)}
				</>
			)}
		</Box>
	);
}

const MessageRow = React.memo(function MessageRow({
	message,
}: {
	message: HistoryMessage;
}) {
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

	if (message.role === 'tool_call') {
		return <ToolCallCell message={message} />;
	}

	if (message.role === 'tool_result') {
		return <ToolResultCell message={message} />;
	}

	return (
		<Box flexDirection="column" marginBottom={1}>
			{message.text ? (
				<Markdown source={message.text} />
			) : (
				<Text color={theme.text}> </Text>
			)}
		</Box>
	);
});
