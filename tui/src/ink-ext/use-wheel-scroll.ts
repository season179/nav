import {useEffect, useRef, useState} from 'react';
import type {Dispatch, SetStateAction} from 'react';
import {useMouseEvents} from './mouse.js';
import type {WheelMouseEvent} from './mouse.js';

type UseWheelScrollOptions = {
	initialScrollTop?: number;
	linesPerWheel?: number;
	maxScrollTop?: number;
	onWheelScroll?: (event: WheelMouseEvent) => void;
	overlayOpen?: boolean;
	throttleMs?: number;
};

type UseWheelScrollResult = {
	scrollTop: number;
	setScrollTop: Dispatch<SetStateAction<number>>;
};

type WheelScrollSettings = Required<
	Pick<
		UseWheelScrollOptions,
		'linesPerWheel' | 'maxScrollTop' | 'overlayOpen' | 'throttleMs'
	>
> & {
	onWheelScroll?: (event: WheelMouseEvent) => void;
};

const DEFAULT_LINES_PER_WHEEL = 3;
const DEFAULT_THROTTLE_MS = 16;

export function useWheelScroll({
	initialScrollTop = 0,
	linesPerWheel = DEFAULT_LINES_PER_WHEEL,
	maxScrollTop = Number.POSITIVE_INFINITY,
	onWheelScroll,
	overlayOpen = false,
	throttleMs = DEFAULT_THROTTLE_MS,
}: UseWheelScrollOptions = {}): UseWheelScrollResult {
	const mouseEvents = useMouseEvents();
	const pendingDeltaRef = useRef(0);
	const timerRef = useRef<ReturnType<typeof setTimeout> | undefined>(
		undefined,
	);
	const settingsRef = useRef<WheelScrollSettings>({
		linesPerWheel,
		maxScrollTop,
		onWheelScroll,
		overlayOpen,
		throttleMs,
	});
	const [scrollTop, setScrollTop] = useState(initialScrollTop);

	settingsRef.current = {
		linesPerWheel,
		maxScrollTop,
		onWheelScroll,
		overlayOpen,
		throttleMs,
	};

	useEffect(() => {
		function flushWheelDelta(): void {
			const delta = pendingDeltaRef.current;
			const {maxScrollTop: latestMaxScrollTop} = settingsRef.current;
			timerRef.current = undefined;
			pendingDeltaRef.current = 0;

			if (delta === 0) {
				return;
			}

			setScrollTop(current => clamp(current + delta, 0, latestMaxScrollTop));
		}

		function onWheel(event: WheelMouseEvent): void {
			const settings = settingsRef.current;
			if (settings.overlayOpen) {
				return;
			}

			settings.onWheelScroll?.(event);
			pendingDeltaRef.current += wheelDelta(event, settings.linesPerWheel);
			if (timerRef.current) {
				return;
			}

			timerRef.current = setTimeout(flushWheelDelta, settings.throttleMs);
		}

		mouseEvents.on('wheel', onWheel);
		return () => {
			mouseEvents.off('wheel', onWheel);
			if (timerRef.current) {
				clearTimeout(timerRef.current);
				timerRef.current = undefined;
			}
		};
	}, [mouseEvents]);

	useEffect(() => {
		setScrollTop(current => clamp(current, 0, maxScrollTop));
	}, [maxScrollTop]);

	return {scrollTop, setScrollTop};
}

function wheelDelta(
	event: WheelMouseEvent,
	linesPerWheel: number,
): number {
	if (event.direction === 'up') {
		return -linesPerWheel;
	}

	return linesPerWheel;
}

function clamp(value: number, min: number, max: number): number {
	return Math.min(max, Math.max(min, Math.floor(value)));
}
