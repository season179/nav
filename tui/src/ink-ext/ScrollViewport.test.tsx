import {describe, expect, test} from 'bun:test';
import {performance} from 'node:perf_hooks';
import {Writable} from 'node:stream';
import React, {useEffect, useState} from 'react';
import {Box, Text, render as renderInk, useStdout} from 'ink';
import {render as renderTest} from 'ink-testing-library';
import {ScrollViewport} from './ScrollViewport.js';
import type {HistoryMessage} from '../regions/history/types.js';

describe('ScrollViewport', () => {
	test('mounts the cropped visible window without a blank top spacer', async () => {
		const renderedIds = new Set<string>();
		const messages = Array.from({length: 8}, (_value, index) =>
			textMessage(index),
		);
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={5}
				onScrollTopChange={() => {}}
				overscan={1}
				estimatedHeight={() => 2}
				renderMessage={message => {
					renderedIds.add(message.id);
					return <FixedHeightCell message={message} />;
				}}
			/>,
		);

		await settle();

		const frame = view.lastFrame() ?? '';
		expect(firstNonEmptyLine(frame)).toContain('message-2 line 2');
		expect(frame).toContain('message-3 line 1');
		expect(frame).toContain('message-4 line 1');
		expect(frame).not.toContain('message-0');
		expect(frame).not.toContain('message-1');
		expect([...renderedIds].sort()).toEqual([
			'message-2',
			'message-3',
			'message-4',
		]);

		view.unmount();
	});

	test('remeasures a row when its content version changes', async () => {
		let messages = [
			versionedMessage('streaming', 3),
			versionedMessage('next', 1),
			versionedMessage('tail', 1),
			versionedMessage('end', 1),
		];
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={3}
				onScrollTopChange={() => {}}
				overscan={1}
				estimatedHeight={() => 3}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);
		await waitForExpectation(() => {
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'streaming line 3',
			);
		});

		messages = [
			versionedMessage('streaming', 10, 2),
			versionedMessage('next', 1),
			versionedMessage('tail', 1),
			versionedMessage('end', 1),
		];
		view.rerender(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={3}
				onScrollTopChange={() => {}}
				overscan={1}
				estimatedHeight={() => 3}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'streaming line 4',
			);
		});

		view.unmount();
	});

	test('clamps scrollTop when viewport rows grow past the content height', async () => {
		const scrollChanges: number[] = [];
		const messages = Array.from({length: 5}, (_value, index) =>
			versionedMessage(`row-${index}`, 1),
		);
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={2}
				scrollTop={3}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
				estimatedHeight={() => 1}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);
		await settle();

		view.rerender(
			<ScrollViewport
				messages={messages}
				viewportHeight={5}
				scrollTop={3}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
				estimatedHeight={() => 1}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(0);
		});

		view.unmount();
	});

	test('clamps scrollTop after a large row deletion even when offscreen rows are still unmeasured', async () => {
		const scrollChanges: number[] = [];
		const messages = Array.from({length: 120}, (_value, index) =>
			versionedMessage(`row-${index}`, 1),
		);
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={110}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
				overscan={0}
				estimatedHeight={() => 1}
				stickyBottom={false}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-110 line 1',
			);
		});

		const remainingMessages = messages.slice(0, 60);
		view.rerender(
			<ScrollViewport
				messages={remainingMessages}
				viewportHeight={4}
				scrollTop={110}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
				overscan={0}
				estimatedHeight={() => 1}
				stickyBottom={false}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(56);
		});
		view.rerender(
			<ScrollViewport
				messages={remainingMessages}
				viewportHeight={4}
				scrollTop={56}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
				overscan={0}
				estimatedHeight={() => 1}
				stickyBottom={false}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);
		expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain('row-56 line 1');

		view.unmount();
	});

	test('keeps sticky bottom pinned as rows append', async () => {
		const scrollChanges: number[] = [];
		let messages = Array.from({length: 5}, (_value, index) =>
			versionedMessage(`row-${index}`, 1),
		);
		const view = renderTest(
			<StickyBottomProbe
				messages={messages}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(2);
		});

		messages = [
			...messages,
			versionedMessage('row-5', 1),
		];
		view.rerender(
			<StickyBottomProbe
				messages={messages}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(3);
		});

		view.unmount();
	});

	test('keeps sticky bottom pinned when an existing row grows after remeasurement', async () => {
		const scrollChanges: number[] = [];
		let messages = Array.from({length: 5}, (_value, index) =>
			versionedMessage(`row-${index}`, 1),
		);
		const view = renderTest(
			<StickyBottomProbe
				messages={messages}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(2);
		});

		messages = [
			...messages.slice(0, 4),
			versionedMessage('row-4', 6, 2),
		];
		view.rerender(
			<StickyBottomProbe
				messages={messages}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(7);
			expect(view.lastFrame()).toContain('row-4 line 6');
		});

		view.unmount();
	});

	test('invalidates cached heights when stdout columns change', async () => {
		const stdout = new CaptureStream(100);
		const stderr = new CaptureStream(100);
		const messages = [
			versionedMessage('wrap', 1),
			versionedMessage('after', 1),
			versionedMessage('tail', 1),
			versionedMessage('end', 1),
		];
		const instance = renderInk(
			<ScrollViewport
				messages={messages}
				viewportHeight={3}
				scrollTop={1}
				onScrollTopChange={() => {}}
				overscan={1}
				estimatedHeight={() => 1}
				renderMessage={message => <WidthSensitiveCell message={message} />}
			/>,
			{
				stdout: stdout as NodeJS.WriteStream,
				stderr: stderr as NodeJS.WriteStream,
				debug: true,
				exitOnCtrlC: false,
				patchConsole: false,
			},
		);

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(stdout.lastFrame())).toContain('after line 1');
		});

		stdout.columns = 40;
		stdout.emit('resize');

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(stdout.lastFrame())).toContain('wrap line 2');
		});

		instance.unmount();
	});

	test('keeps the first-scroll anchor stable through a long wide-character row', async () => {
		const messages = [
			versionedMessage('漢字🙂 long', 50),
			versionedMessage('after', 1),
		];
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={5}
				scrollTop={4}
				onScrollTopChange={() => {}}
				stickyBottom={false}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		const firstFrameLine = firstNonEmptyLine(view.lastFrame() ?? '');
		await settle();

		expect(firstFrameLine).toContain('漢字🙂 long line 5');
		expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
			'漢字🙂 long line 5',
		);

		view.unmount();
	});

	test('preserves the visible anchor when a measured row grows above the viewport', async () => {
		let messages = Array.from({length: 8}, (_value, index) =>
			versionedMessage(`row-${index}`, 1),
		);
		const scrollChanges: number[] = [];
		const view = renderTest(
			<AnchoredScrollProbe
				messages={messages}
				initialScrollTop={3}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-3 line 1',
			);
		});

		messages = [
			versionedMessage('row-0', 5, 2),
			...messages.slice(1),
		];
		view.rerender(
			<AnchoredScrollProbe
				messages={messages}
				initialScrollTop={3}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(7);
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-3 line 1',
			);
		});

		view.unmount();
	});

	test('accumulates visible-anchor corrections from multiple growing rows', async () => {
		let messages = Array.from({length: 8}, (_value, index) =>
			versionedMessage(`row-${index}`, 1),
		);
		const scrollChanges: number[] = [];
		const view = renderTest(
			<AnchoredScrollProbe
				messages={messages}
				initialScrollTop={3}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-3 line 1',
			);
		});

		messages = [
			versionedMessage('row-0', 5, 2),
			versionedMessage('row-1', 4, 2),
			...messages.slice(2),
		];
		view.rerender(
			<AnchoredScrollProbe
				messages={messages}
				initialScrollTop={3}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(10);
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-3 line 1',
			);
		});

		view.unmount();
	});

	test('does not overcorrect when a partially visible row grows with an earlier row', async () => {
		let messages = [
			versionedMessage('row-0', 5),
			versionedMessage('row-1', 5),
			...Array.from({length: 6}, (_value, index) =>
				versionedMessage(`row-${index + 2}`, 1),
			),
		];
		const scrollChanges: number[] = [];
		const view = renderTest(
			<AnchoredScrollProbe
				messages={messages}
				initialScrollTop={6}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-1 line 2',
			);
		});

		messages = [
			versionedMessage('row-0', 6, 2),
			versionedMessage('row-1', 8, 2),
			...messages.slice(2),
		];
		view.rerender(
			<AnchoredScrollProbe
				messages={messages}
				initialScrollTop={6}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges).toContain(7);
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
				'row-1 line 2',
			);
		});
		await settle();
		expect(scrollChanges).not.toContain(10);
		expect(firstNonEmptyLine(view.lastFrame() ?? '')).toContain(
			'row-1 line 2',
		);

		view.unmount();
	});

	test('keeps first-scroll anchors stable through five 48+ row cells with the default estimate', async () => {
		const messages = Array.from({length: 5}, (_value, index) =>
			versionedMessage(`漢字🙂 long-${index}`, 50),
		);
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={5}
				scrollTop={4}
				onScrollTopChange={() => {}}
				stickyBottom={false}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);
		const scrollSteps = [4, 52, 100, 152, 200];

		for (const scrollTop of scrollSteps) {
			view.rerender(
				<ScrollViewport
					messages={messages}
					viewportHeight={5}
					scrollTop={scrollTop}
					onScrollTopChange={() => {}}
					stickyBottom={false}
					renderMessage={message => <VariableHeightCell message={message} />}
				/>,
			);
			const beforeMeasure = firstNonEmptyLine(view.lastFrame() ?? '');
			await settle();

			expect(beforeMeasure).toContain('漢字🙂 long-');
			expect(firstNonEmptyLine(view.lastFrame() ?? '')).toBe(beforeMeasure);
		}

		view.unmount();
	});

	test('keeps action-to-render latency inside the production gate', async () => {
		const messages = Array.from({length: 160}, (_value, index) =>
			versionedMessage(`perf-${index}`, 3),
		);
		const virtualized = await measureScrollLatency('virtualized', messages);
		const renderAll = await measureScrollLatency('render-all', messages);

		console.info(
			[
				'ScrollViewport e2e latency',
				`virtualized median ${virtualized.median.toFixed(2)}ms`,
				`p95 ${virtualized.p95.toFixed(2)}ms`,
				`max ${virtualized.max.toFixed(2)}ms`,
				'| render-all control',
				`median ${renderAll.median.toFixed(2)}ms`,
				`p95 ${renderAll.p95.toFixed(2)}ms`,
				`max ${renderAll.max.toFixed(2)}ms`,
			].join(' '),
		);

		expect(virtualized.median).toBeLessThanOrEqual(8);
		expect(virtualized.p95).toBeLessThanOrEqual(16);
		expect(virtualized.max).toBeLessThanOrEqual(30);
	});
});

describe('ScrollViewport follow-tail streaming', () => {
	test('auto-scrolls to bottom when content grows while at bottom', async () => {
		const scrollChanges: number[] = [];
		let messages = [
			versionedMessage('stream-1', 3),
			versionedMessage('stream-2', 3),
		];
		const view = renderTest(
			<StickyBottomProbe
				messages={messages}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges.length).toBeGreaterThan(0);
		});

		// Grow the last message with more content
		messages = [
			versionedMessage('stream-1', 3),
			versionedMessage('stream-2', 10, 2),
		];
		view.rerender(
			<StickyBottomProbe
				messages={messages}
				onScrollTopChange={next => {
					scrollChanges.push(next);
				}}
			/>,
		);

		await waitForExpectation(() => {
			const maxScrollTop = scrollChanges.at(-1)!;
			expect(maxScrollTop).toBeGreaterThan(0);
		});

		view.unmount();
	});

	test('stops auto-following when user scrolls up', async () => {
		const scrollChanges: number[] = [];
		let scrollTop = 0;
		let messages = Array.from({length: 10}, (_, index) =>
			versionedMessage(`msg-${index}`, 2),
		);
		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={scrollTop}
				onScrollTopChange={next => {
					scrollTop = next;
					scrollChanges.push(next);
				}}
				stickyBottom={true}
				estimatedHeight={() => 2}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges.length).toBeGreaterThan(0);
		});

		// Simulate scrolling up by setting stickyBottom to false
		scrollTop = 5;
		view.rerender(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={scrollTop}
				onScrollTopChange={next => {
					scrollTop = next;
					scrollChanges.push(next);
				}}
				stickyBottom={false}
				estimatedHeight={() => 2}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await settle();

		// Add more content - should NOT auto-scroll
		messages = [
			...messages,
			versionedMessage('msg-10', 2),
		];
		view.rerender(
			<ScrollViewport
				messages={messages}
				viewportHeight={4}
				scrollTop={scrollTop}
				onScrollTopChange={next => {
					scrollTop = next;
					scrollChanges.push(next);
				}}
				stickyBottom={false}
				estimatedHeight={() => 2}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await settle();
		expect(scrollTop).toBe(5);

		view.unmount();
	});

	test('re-enables follow-tail when scrollTop reaches maxScrollTop', async () => {
		const scrollChanges: number[] = [];
		let scrollTop = 0;
		let stickyBottom = true;
		let messages = Array.from({length: 5}, (_, index) =>
			versionedMessage(`msg-${index}`, 2),
		);

		function rerenderViewport(): void {
			view.rerender(
				<ScrollViewport
					messages={messages}
					viewportHeight={3}
					scrollTop={scrollTop}
					onScrollTopChange={next => {
						scrollTop = next;
						scrollChanges.push(next);
					}}
					stickyBottom={stickyBottom}
					estimatedHeight={() => 2}
					renderMessage={message => <VariableHeightCell message={message} />}
				/>,
			);
		}

		const view = renderTest(
			<ScrollViewport
				messages={messages}
				viewportHeight={3}
				scrollTop={scrollTop}
				onScrollTopChange={next => {
					scrollTop = next;
					scrollChanges.push(next);
				}}
				stickyBottom={stickyBottom}
				estimatedHeight={() => 2}
				renderMessage={message => <VariableHeightCell message={message} />}
			/>,
		);

		await waitForExpectation(() => {
			expect(scrollChanges.length).toBeGreaterThan(0);
		});

		// Scroll up
		scrollTop = 2;
		stickyBottom = false;
		rerenderViewport();

		await settle();

		// Scroll back to bottom
		stickyBottom = true;
		rerenderViewport();

		await waitForExpectation(() => {
			expect(scrollChanges.some(c => c >= 4)).toBe(true);
		});

		// Add new content - should auto-scroll
		messages = [
			...messages,
			versionedMessage('msg-5', 2),
		];
		rerenderViewport();

		await waitForExpectation(() => {
			expect(scrollChanges.some(c => c >= 6)).toBe(true);
		});

		view.unmount();
	});
});

function FixedHeightCell({
	message,
}: {
	message: HistoryMessage;
}): React.JSX.Element {
	return (
		<Box flexDirection="column">
			<Text>{message.id} line 1</Text>
			<Text>{message.id} line 2</Text>
		</Box>
	);
}

function textMessage(index: number): HistoryMessage {
	return {
		id: `message-${index}`,
		role: 'assistant',
		text: `message ${index}`,
	};
}

function versionedMessage(
	id: string,
	lines: number,
	contentVersion = 1,
): HistoryMessage {
	return {
		id,
		role: 'assistant',
		text: Array.from(
			{length: lines},
			(_value, index) => `${id} line ${index + 1}`,
		).join('\n'),
		contentVersion,
	};
}

function VariableHeightCell({
	message,
}: {
	message: HistoryMessage;
}): React.JSX.Element {
	const lines = 'text' in message ? message.text.split('\n') : [];
	return (
		<Box flexDirection="column">
			{lines.map(line => (
				<Text key={line}>{line}</Text>
			))}
		</Box>
	);
}

function StickyBottomProbe({
	messages,
	onScrollTopChange,
}: {
	messages: HistoryMessage[];
	onScrollTopChange: (scrollTop: number) => void;
}): React.JSX.Element {
	const [scrollTop, setScrollTop] = useState(0);
	return (
		<ScrollViewport
			messages={messages}
			viewportHeight={3}
			scrollTop={scrollTop}
			onScrollTopChange={next => {
				onScrollTopChange(next);
				setScrollTop(next);
			}}
			estimatedHeight={() => 1}
			renderMessage={message => <VariableHeightCell message={message} />}
		/>
	);
}

function AnchoredScrollProbe({
	messages,
	initialScrollTop,
	onScrollTopChange,
}: {
	messages: HistoryMessage[];
	initialScrollTop: number;
	onScrollTopChange: (scrollTop: number) => void;
}): React.JSX.Element {
	const [scrollTop, setScrollTop] = useState(initialScrollTop);
	return (
		<ScrollViewport
			messages={messages}
			viewportHeight={3}
			scrollTop={scrollTop}
			onScrollTopChange={next => {
				onScrollTopChange(next);
				setScrollTop(next);
			}}
			overscan={10}
			stickyBottom={false}
			estimatedHeight={() => 1}
			renderMessage={message => <VariableHeightCell message={message} />}
		/>
	);
}

function WidthSensitiveCell({
	message,
}: {
	message: HistoryMessage;
}): React.JSX.Element {
	const {stdout} = useStdout();
	const lineCount = message.id === 'wrap' && stdout.columns < 50 ? 4 : 1;
	return (
		<Box flexDirection="column">
			{Array.from({length: lineCount}, (_value, index) => (
				<Text key={index}>
					{message.id} line {index + 1}
				</Text>
			))}
		</Box>
	);
}

class CaptureStream extends Writable {
	columns: number;
	rows = 24;
	isTTY = true;
	private readonly frames: string[] = [];

	constructor(columns: number) {
		super();
		this.columns = columns;
	}

	lastFrame(): string {
		return this.frames.at(-1) ?? '';
	}

	_write(
		chunk: Buffer,
		_encoding: BufferEncoding,
		callback: (error?: Error | null) => void,
	): void {
		this.frames.push(chunk.toString());
		callback();
	}
}

type PerfMode = 'virtualized' | 'render-all';

type LatencyStats = {
	median: number;
	p95: number;
	max: number;
};

async function measureScrollLatency(
	mode: PerfMode,
	messages: HistoryMessage[],
): Promise<LatencyStats> {
	const stdout = new CaptureStream(100);
	const stderr = new CaptureStream(100);
	let step: (() => void) | undefined;
	let actionStartedAt: number | undefined;
	let resolveRender: (() => void) | undefined;
	const latencies: number[] = [];
	const instance = renderInk(
		<PerfProbe
			mode={mode}
			messages={messages}
			onReady={nextStep => {
				step = nextStep;
			}}
		/>,
		{
			stdout: stdout as NodeJS.WriteStream,
			stderr: stderr as NodeJS.WriteStream,
			debug: true,
			exitOnCtrlC: false,
			patchConsole: false,
			maxFps: 1000,
			onRender() {
				if (actionStartedAt === undefined) {
					return;
				}

				latencies.push(performance.now() - actionStartedAt);
				actionStartedAt = undefined;
				resolveRender?.();
				resolveRender = undefined;
			},
		},
	);

	await waitForExpectation(() => {
		expect(step).toBeDefined();
	});
	await settle();

	for (let index = 0; index < 35; index += 1) {
		await new Promise<void>(resolve => {
			resolveRender = resolve;
			actionStartedAt = performance.now();
			step?.();
		});
	}

	instance.unmount();
	return latencyStats(latencies.slice(5));
}

function PerfProbe({
	mode,
	messages,
	onReady,
}: {
	mode: PerfMode;
	messages: HistoryMessage[];
	onReady: (step: () => void) => void;
}): React.JSX.Element {
	const [scrollTop, setScrollTop] = useState(0);

	useEffect(() => {
		onReady(() => {
			setScrollTop(current => current + 1);
		});
	}, [onReady]);

	if (mode === 'render-all') {
		return (
			<RenderAllViewport
				messages={messages}
				viewportHeight={12}
				scrollTop={scrollTop}
			/>
		);
	}

	return (
		<ScrollViewport
			messages={messages}
			viewportHeight={12}
			scrollTop={scrollTop}
			onScrollTopChange={setScrollTop}
			stickyBottom={false}
			estimatedHeight={() => 3}
			renderMessage={message => <VariableHeightCell message={message} />}
		/>
	);
}

function RenderAllViewport({
	messages,
	viewportHeight,
	scrollTop,
}: {
	messages: HistoryMessage[];
	viewportHeight: number;
	scrollTop: number;
}): React.JSX.Element {
	return (
		<Box height={viewportHeight} overflow="hidden" flexDirection="column">
			<Box flexDirection="column" flexShrink={0} marginTop={-scrollTop}>
				{messages.map(message => (
					<Box key={message.id} flexDirection="column" flexShrink={0}>
						<VariableHeightCell message={message} />
					</Box>
				))}
			</Box>
		</Box>
	);
}

function latencyStats(latencies: number[]): LatencyStats {
	const sorted = [...latencies].sort((left, right) => left - right);
	return {
		median: percentile(sorted, 0.5),
		p95: percentile(sorted, 0.95),
		max: sorted.at(-1) ?? 0,
	};
}

function percentile(sorted: number[], quantile: number): number {
	if (sorted.length === 0) {
		return 0;
	}

	const index = Math.min(
		sorted.length - 1,
		Math.floor((sorted.length - 1) * quantile),
	);
	return sorted[index]!;
}

function firstNonEmptyLine(frame: string): string {
	return frame.split('\n').find(line => line.trim().length > 0) ?? '';
}

async function settle(): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, 10));
}

async function waitForExpectation(expectation: () => void): Promise<void> {
	const deadline = Date.now() + 250;
	let lastError: unknown;

	while (Date.now() < deadline) {
		try {
			expectation();
			return;
		} catch (error) {
			lastError = error;
			await settle();
		}
	}

	if (lastError) {
		throw lastError;
	}
	expectation();
}
