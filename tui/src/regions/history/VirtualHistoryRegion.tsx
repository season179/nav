import React, {useEffect, useRef, useState} from 'react';
import {Box, Text} from 'ink';
import {FileChangedCell} from './FileChangedCell.js';
import {ToolCallCell} from './ToolCallCell.js';
import {ToolResultCell} from './ToolResultCell.js';
import type {HistoryMessage} from './types.js';
import {Markdown} from '../../markdown/Markdown.js';
import {theme} from '../../theme/index.js';
import {ScrollViewport} from '../../ink-ext/ScrollViewport.js';
import {useWheelScroll} from '../../ink-ext/use-wheel-scroll.js';

const FOLLOW_TAIL_TOLERANCE = 1;

type Props = {
	messages: HistoryMessage[];
	height: number;
};

export function VirtualHistoryRegion({
	messages,
	height,
}: Props): React.JSX.Element {
	const [maxScrollTop, setMaxScrollTop] = useState<number | undefined>();
	const [stickyBottom, setStickyBottom] = useState(true);
	const knownMaxScrollTop = maxScrollTop ?? 0;
	const {scrollTop, setScrollTop} = useWheelScroll({
		maxScrollTop: maxScrollTop ?? Number.POSITIVE_INFINITY,
		onWheelScroll: event => {
			if (event.direction === 'up' && scrollTop > 0) {
				setStickyBottom(false);
			}
		},
	});
	const previousScrollTopRef = useRef(scrollTop);
	const atBottom = isAtBottom(scrollTop, knownMaxScrollTop);

	useEffect(() => {
		const previousScrollTop = previousScrollTopRef.current;
		previousScrollTopRef.current = scrollTop;

		if (scrollTop !== previousScrollTop && atBottom) {
			setStickyBottom(true);
		}
	}, [atBottom, scrollTop]);

	useEffect(() => {
		if (stickyBottom && !atBottom) {
			setScrollTop(knownMaxScrollTop);
		}
	}, [atBottom, knownMaxScrollTop, setScrollTop, stickyBottom]);

	const indicatorVisible = scrollTop < knownMaxScrollTop;
	const viewportHeight = Math.max(1, height - (indicatorVisible ? 1 : 0));
	const hiddenRows = Math.max(0, knownMaxScrollTop - scrollTop);

	return (
		<Box
			flexDirection="column"
			flexGrow={1}
			paddingX={2}
			paddingY={0}
			justifyContent="flex-end"
		>
			{messages.length === 0 ? (
				<Box marginTop={1}>
					<Text color={theme.inactive}>
						Ask a question, or type /model or /exit.
					</Text>
				</Box>
			) : (
				<>
					<ScrollViewport
						messages={messages}
						renderMessage={message => <MessageRow message={message} />}
						scrollTop={scrollTop}
						onScrollTopChange={nextScrollTop => {
							setScrollTop(nextScrollTop);
							if (isAtBottom(nextScrollTop, knownMaxScrollTop)) {
								setStickyBottom(true);
							}
						}}
						onScrollMetricsChange={({maxScrollTop: nextMaxScrollTop}) => {
							setMaxScrollTop(current =>
								current === nextMaxScrollTop ? current : nextMaxScrollTop,
							);
						}}
						viewportHeight={viewportHeight}
						stickyBottom={stickyBottom}
					/>
					{indicatorVisible ? (
						<Box justifyContent="center">
							<Text color={theme.inactive}>
								↓ {hiddenRows} hidden · scroll to reveal
							</Text>
						</Box>
					) : null}
				</>
			)}
		</Box>
	);
}

function isAtBottom(scrollTop: number, maxScrollTop: number): boolean {
	return scrollTop >= maxScrollTop - FOLLOW_TAIL_TOLERANCE;
}

const MessageRow = React.memo(function MessageRow({
	message,
}: {
	message: HistoryMessage;
}): React.JSX.Element {
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

	if (message.role === 'file_changed') {
		return <FileChangedCell message={message} />;
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
