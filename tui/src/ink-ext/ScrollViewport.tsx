import React, {
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from 'react';
import {Box, measureElement, useStdout} from 'ink';
import type {DOMElement} from 'ink';
import type {HistoryMessage} from '../regions/history/types.js';

type Props = {
	messages: HistoryMessage[];
	renderMessage: (message: HistoryMessage) => React.ReactNode;
	estimatedHeight?: (message: HistoryMessage) => number;
	scrollTop: number;
	onScrollTopChange: (scrollTop: number) => void;
	onScrollMetricsChange?: (metrics: ScrollMetrics) => void;
	viewportHeight: number;
	stickyBottom?: boolean;
	overscan?: number;
};

export type ScrollMetrics = {
	maxScrollTop: number;
	totalHeight: number;
};

type LayoutItem = {
	key: string;
	message: HistoryMessage;
	top: number;
	height: number;
};

type ViewportLayout = {
	items: LayoutItem[];
	totalHeight: number;
	maxScrollTop: number;
	hasUnmeasuredRows: boolean;
};

const DEFAULT_ESTIMATED_HEIGHT = 4;
const DEFAULT_OVERSCAN = 5;
const DEFAULT_VIEWPORT_WIDTH = 80;
const STICKY_BOTTOM_TOLERANCE = 1;

export function ScrollViewport({
	messages,
	renderMessage,
	estimatedHeight = defaultEstimatedHeight,
	scrollTop,
	onScrollTopChange,
	onScrollMetricsChange,
	viewportHeight,
	stickyBottom = true,
	overscan = DEFAULT_OVERSCAN,
}: Props): React.JSX.Element {
	const viewportWidth = useViewportWidth();
	const heightCacheRef = useRef(new Map<string, number>());
	const cachedWidthRef = useRef(viewportWidth);
	const previousMaxScrollTopRef = useRef(0);
	const [measureRevision, setMeasureRevision] = useState(0);

	if (cachedWidthRef.current !== viewportWidth) {
		heightCacheRef.current.clear();
		cachedWidthRef.current = viewportWidth;
	}

	const layout = useMemo(
		() =>
			buildLayout({
				messages,
				viewportWidth,
				viewportHeight,
				heightCache: heightCacheRef.current,
				estimatedHeight,
			}),
		[
			estimatedHeight,
			messages,
			measureRevision,
			viewportHeight,
			viewportWidth,
		],
	);
	const renderMaxScrollTop = layout.hasUnmeasuredRows
		? Math.max(scrollTop, layout.maxScrollTop)
		: layout.maxScrollTop;
	const clampedScrollTop = clamp(scrollTop, 0, renderMaxScrollTop);
	const window = getMountedWindow({
		items: layout.items,
		scrollTop: clampedScrollTop,
		viewportHeight,
		overscan,
	});
	const cropRows = clampedScrollTop - window.top;

	useLayoutEffect(() => {
		const previousMaxScrollTop = previousMaxScrollTopRef.current;
		const wasAtBottom =
			scrollTop >= previousMaxScrollTop - STICKY_BOTTOM_TOLERANCE;
		const missingRequestedWindow =
			window.items.length === 0 && scrollTop > layout.maxScrollTop;
		let nextScrollTop = layout.hasUnmeasuredRows && !missingRequestedWindow
			? Math.max(0, scrollTop)
			: clamp(scrollTop, 0, layout.maxScrollTop);

		if (stickyBottom && wasAtBottom) {
			nextScrollTop = layout.maxScrollTop;
		}

		if (nextScrollTop !== scrollTop) {
			onScrollTopChange(nextScrollTop);
		}

		previousMaxScrollTopRef.current = layout.maxScrollTop;
	}, [
		layout.maxScrollTop,
		layout.hasUnmeasuredRows,
		onScrollTopChange,
		scrollTop,
		stickyBottom,
		window.items.length,
	]);

	useLayoutEffect(() => {
		onScrollMetricsChange?.({
			maxScrollTop: layout.maxScrollTop,
			totalHeight: layout.totalHeight,
		});
	}, [layout.maxScrollTop, layout.totalHeight, onScrollMetricsChange]);

	useLayoutEffect(() => {
		pruneHeightCache(heightCacheRef.current, messages, viewportWidth);
	}, [messages, viewportWidth]);

	const handleMeasure = useCallback((cacheKey: string, height: number) => {
		if (height <= 0) {
			return;
		}

		const cache = heightCacheRef.current;
		if (cache.get(cacheKey) === height) {
			return;
		}

		cache.set(cacheKey, height);
		setMeasureRevision(revision => revision + 1);
	}, []);

	return (
		<Box
			flexDirection="column"
			height={viewportHeight}
			overflow="hidden"
			flexShrink={0}
			justifyContent={
				layout.totalHeight < viewportHeight ? 'flex-end' : 'flex-start'
			}
		>
			<Box flexDirection="column" marginTop={-cropRows} flexShrink={0}>
				{window.items.map(item => (
					<MeasuredRow
						key={item.key}
						cacheKey={item.key}
						onMeasure={handleMeasure}
					>
						{renderMessage(item.message)}
					</MeasuredRow>
				))}
			</Box>
		</Box>
	);
}

function MeasuredRow({
	cacheKey,
	children,
	onMeasure,
}: {
	cacheKey: string;
	children: React.ReactNode;
	onMeasure: (cacheKey: string, height: number) => void;
}): React.JSX.Element {
	const ref = useRef<DOMElement | null>(null);

	useLayoutEffect(() => {
		if (!ref.current) {
			return;
		}

		onMeasure(cacheKey, measureElement(ref.current).height);
	});

	return (
		<Box ref={ref} flexDirection="column" flexShrink={0}>
			{children}
		</Box>
	);
}

function buildLayout({
	messages,
	viewportWidth,
	viewportHeight,
	heightCache,
	estimatedHeight,
}: {
	messages: HistoryMessage[];
	viewportWidth: number;
	viewportHeight: number;
	heightCache: Map<string, number>;
	estimatedHeight: (message: HistoryMessage) => number;
}): ViewportLayout {
	let totalHeight = 0;
	let hasUnmeasuredRows = false;
	const items = messages.map(message => {
		const key = cacheKey(message, viewportWidth);
		const cachedHeight = heightCache.get(key);
		const height =
			cachedHeight ?? normalizedEstimatedHeight(message, estimatedHeight);
		hasUnmeasuredRows ||= cachedHeight === undefined;
		const item = {
			key,
			message,
			top: totalHeight,
			height,
		};
		totalHeight += height;
		return item;
	});

	return {
		items,
		totalHeight,
		maxScrollTop: Math.max(0, totalHeight - viewportHeight),
		hasUnmeasuredRows,
	};
}

function getMountedWindow({
	items,
	scrollTop,
	viewportHeight,
	overscan,
}: {
	items: LayoutItem[];
	scrollTop: number;
	viewportHeight: number;
	overscan: number;
}): {items: LayoutItem[]; top: number} {
	const windowStart = Math.max(0, scrollTop - overscan);
	const windowEnd = scrollTop + viewportHeight + overscan;
	const startIndex = items.findIndex(
		item => item.top + item.height > windowStart,
	);

	if (startIndex === -1) {
		return {items: [], top: 0};
	}

	let endIndex = startIndex;
	while (endIndex < items.length && items[endIndex]!.top < windowEnd) {
		endIndex += 1;
	}

	return {
		items: items.slice(startIndex, endIndex),
		top: items[startIndex]!.top,
	};
}

function useViewportWidth(): number {
	const {stdout} = useStdout();
	const [width, setWidth] = useState(
		() => stdout.columns ?? DEFAULT_VIEWPORT_WIDTH,
	);

	useEffect(() => {
		const updateWidth = () => {
			setWidth(stdout.columns ?? DEFAULT_VIEWPORT_WIDTH);
		};

		updateWidth();
		stdout.on('resize', updateWidth);
		return () => {
			stdout.off('resize', updateWidth);
		};
	}, [stdout]);

	return width;
}

function cacheKey(message: HistoryMessage, viewportWidth: number): string {
	return JSON.stringify([
		message.id,
		viewportWidth,
		messageContentVersion(message),
	]);
}

function messageContentVersion(message: HistoryMessage): number {
	return message.contentVersion ?? 0;
}

function pruneHeightCache(
	heightCache: Map<string, number>,
	messages: HistoryMessage[],
	viewportWidth: number,
): void {
	const activeKeys = new Set(
		messages.map(message => cacheKey(message, viewportWidth)),
	);
	for (const key of heightCache.keys()) {
		if (!activeKeys.has(key)) {
			heightCache.delete(key);
		}
	}
}

function normalizedEstimatedHeight(
	message: HistoryMessage,
	estimatedHeight: (message: HistoryMessage) => number,
): number {
	return Math.max(1, Math.ceil(estimatedHeight(message)));
}

function defaultEstimatedHeight(): number {
	return DEFAULT_ESTIMATED_HEIGHT;
}

function clamp(value: number, min: number, max: number): number {
	return Math.min(max, Math.max(min, Math.floor(value)));
}
