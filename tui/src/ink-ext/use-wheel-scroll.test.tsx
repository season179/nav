import {describe, expect, test} from 'bun:test';
import {EventEmitter} from 'node:events';
import React from 'react';
import {Text} from 'ink';
import {render} from 'ink-testing-library';
import {MouseEventProvider} from './mouse.js';
import {useWheelScroll} from './use-wheel-scroll.js';
import type {WheelMouseEvent} from './mouse.js';

describe('useWheelScroll', () => {
	test('applies wheel deltas after the 16ms throttle window', async () => {
		const emitter = new EventEmitter();
		const view = render(
			<MouseEventProvider emitter={emitter}>
				<WheelScrollProbe />
			</MouseEventProvider>,
		);

		emitter.emit('wheel', wheelEvent('down'));
		await wait(5);
		expect(view.lastFrame()).toBe('0');

		await wait(20);
		expect(view.lastFrame()).toBe('3');

		view.unmount();
	});

	test('ignores wheel events while an overlay is open', async () => {
		const emitter = new EventEmitter();
		const view = render(
			<MouseEventProvider emitter={emitter}>
				<WheelScrollProbe overlayOpen />
			</MouseEventProvider>,
		);

		emitter.emit('wheel', wheelEvent('down'));
		await wait(25);

		expect(view.lastFrame()).toBe('0');

		view.unmount();
	});

	test('does not lose a pending wheel delta when options change during the throttle window', async () => {
		const emitter = new EventEmitter();
		const view = render(
			<MouseEventProvider emitter={emitter}>
				<WheelScrollProbe maxScrollTop={10} />
			</MouseEventProvider>,
		);

		emitter.emit('wheel', wheelEvent('down'));
		await wait(5);
		view.rerender(
			<MouseEventProvider emitter={emitter}>
				<WheelScrollProbe maxScrollTop={20} />
			</MouseEventProvider>,
		);

		await wait(25);
		expect(view.lastFrame()).toBe('3');

		view.unmount();
	});

	test('clamps owned scrollTop when the maximum scroll offset shrinks', async () => {
		const emitter = new EventEmitter();
		const view = render(
			<MouseEventProvider emitter={emitter}>
				<WheelScrollProbe initialScrollTop={8} maxScrollTop={10} />
			</MouseEventProvider>,
		);

		expect(view.lastFrame()).toBe('8');

		view.rerender(
			<MouseEventProvider emitter={emitter}>
				<WheelScrollProbe initialScrollTop={8} maxScrollTop={5} />
			</MouseEventProvider>,
		);

		await wait(5);
		expect(view.lastFrame()).toBe('5');

		view.unmount();
	});
});

function WheelScrollProbe({
	initialScrollTop = 0,
	maxScrollTop = Number.POSITIVE_INFINITY,
	overlayOpen = false,
}: {
	initialScrollTop?: number;
	maxScrollTop?: number;
	overlayOpen?: boolean;
}): React.JSX.Element {
	const {scrollTop} = useWheelScroll({
		initialScrollTop,
		maxScrollTop,
		overlayOpen,
	});
	return <Text>{scrollTop}</Text>;
}

function wheelEvent(direction: WheelMouseEvent['direction']): WheelMouseEvent {
	return {
		type: 'wheel',
		direction,
		ctrl: false,
		shift: false,
		alt: false,
	};
}

async function wait(milliseconds: number): Promise<void> {
	await new Promise(resolve => setTimeout(resolve, milliseconds));
}
