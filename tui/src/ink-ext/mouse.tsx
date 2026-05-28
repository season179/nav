import {EventEmitter} from 'node:events';
import {Readable} from 'node:stream';
import React, {createContext, useContext} from 'react';

type StdinLike = NodeJS.ReadableStream & {
	isTTY?: boolean;
	isRaw?: boolean;
	setRawMode?: (enabled: boolean) => unknown;
	ref?: () => unknown;
	unref?: () => unknown;
};

export type StdinProxy = {
	proxy: NodeJS.ReadStream;
	mouseEvents: EventEmitter;
	dispose: () => void;
};

export type WheelMouseEvent = {
	type: 'wheel';
	direction: 'up' | 'down';
	ctrl: boolean;
	shift: boolean;
	alt: boolean;
};

export type ButtonMouseEvent = {
	type: 'mouse';
	action: 'press' | 'release';
	button: number;
	ctrl: boolean;
	shift: boolean;
	alt: boolean;
};

type ParsedMouseEvent = WheelMouseEvent | ButtonMouseEvent;
type ByteBuffer = Buffer<ArrayBufferLike>;

const PARTIAL_FLUSH_MS = 25;
const SGR_MOUSE_PREFIX = [0x1b, 0x5b, 0x3c] as const;
const NOOP_MOUSE_EVENTS = new EventEmitter();

type MouseEventProviderProps = {
	emitter: EventEmitter;
	children: React.ReactNode;
};

const MouseEventContext = createContext<EventEmitter | null>(null);

export function MouseEventProvider({
	emitter,
	children,
}: MouseEventProviderProps): React.JSX.Element {
	return (
		<MouseEventContext.Provider value={emitter}>
			{children}
		</MouseEventContext.Provider>
	);
}

export function useMouseEvents(): EventEmitter {
	return useContext(MouseEventContext) ?? NOOP_MOUSE_EVENTS;
}

export function createStdinProxy(stdin: NodeJS.ReadableStream): StdinProxy {
	const source = stdin as StdinLike;
	const proxy = new DelegatingReadable(source);
	const mouseEvents = new EventEmitter();
	let pending: ByteBuffer = Buffer.alloc(0);
	let flushTimer: ReturnType<typeof setTimeout> | undefined;
	let disposed = false;

	function onData(chunk: string | Buffer): void {
		clearFlushTimer();
		const input = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
		const parsed = demuxMouseBytes(Buffer.concat([pending, input]), mouseEvents);
		pending = parsed.pending;
		const passthrough = parsed.passthrough;
		if (passthrough.length > 0) {
			proxy.push(passthrough);
		}
		if (pending.length > 0) {
			flushTimer = setTimeout(flushPending, PARTIAL_FLUSH_MS);
		}
	}

	function clearFlushTimer(): void {
		if (!flushTimer) {
			return;
		}
		clearTimeout(flushTimer);
		flushTimer = undefined;
	}

	function flushPending(): void {
		flushTimer = undefined;
		if (pending.length === 0) {
			return;
		}
		proxy.push(pending);
		pending = Buffer.alloc(0);
	}

	source.on('data', onData);

	return {
		proxy: proxy as unknown as NodeJS.ReadStream,
		mouseEvents,
		dispose() {
			if (disposed) {
				return;
			}
			disposed = true;
			clearFlushTimer();
			source.off('data', onData);
			flushPending();
			proxy.push(null);
		},
	};
}

function demuxMouseBytes(
	input: ByteBuffer,
	mouseEvents: EventEmitter,
): {passthrough: ByteBuffer; pending: ByteBuffer} {
	const passthrough: ByteBuffer[] = [];
	let cursor = 0;

	while (cursor < input.length) {
		const escapeStart = input.indexOf(0x1b, cursor);
		if (escapeStart === -1) {
			passthrough.push(input.subarray(cursor));
			break;
		}

		if (escapeStart > cursor) {
			passthrough.push(input.subarray(cursor, escapeStart));
		}

		const prefix = sgrPrefixState(input, escapeStart);
		if (prefix === 'partial') {
			return {
				passthrough: Buffer.concat(passthrough),
				pending: input.subarray(escapeStart),
			};
		}
		if (prefix === 'none') {
			passthrough.push(input.subarray(escapeStart, escapeStart + 1));
			cursor = escapeStart + 1;
			continue;
		}

		const sgrEnd = findSgrTerminator(input, escapeStart + 3);
		if (sgrEnd === -1) {
			return {
				passthrough: Buffer.concat(passthrough),
				pending: input.subarray(escapeStart),
			};
		}

		const event = parseMouseEvent(input.subarray(escapeStart, sgrEnd + 1));
		if (event) {
			mouseEvents.emit(event.type, event);
		} else {
			passthrough.push(input.subarray(escapeStart, sgrEnd + 1));
		}
		cursor = sgrEnd + 1;
	}

	return {passthrough: Buffer.concat(passthrough), pending: Buffer.alloc(0)};
}

function sgrPrefixState(
	input: ByteBuffer,
	start: number,
): 'full' | 'partial' | 'none' {
	const remaining = input.length - start;
	const compared = Math.min(remaining, SGR_MOUSE_PREFIX.length);
	for (let offset = 0; offset < compared; offset += 1) {
		if (input[start + offset] !== SGR_MOUSE_PREFIX[offset]) {
			return 'none';
		}
	}
	return remaining < SGR_MOUSE_PREFIX.length ? 'partial' : 'full';
}

function findSgrTerminator(input: ByteBuffer, start: number): number {
	for (let index = start; index < input.length; index += 1) {
		const byte = input[index];
		if (byte === 0x4d || byte === 0x6d) {
			return index;
		}
	}
	return -1;
}

function parseMouseEvent(sequence: ByteBuffer): ParsedMouseEvent | null {
	const match = /^\x1B\[<(\d+);\d+;\d+([Mm])$/.exec(
		sequence.toString('ascii'),
	);
	if (!match) {
		return null;
	}

	const buttonCode = Number(match[1]);
	const modifiers = decodeModifiers(buttonCode);
	if ((buttonCode & 64) === 0) {
		return {
			type: 'mouse',
			action: match[2] === 'm' ? 'release' : 'press',
			button: buttonCode & 3,
			...modifiers,
		};
	}

	return {
		type: 'wheel',
		direction: (buttonCode & 1) === 0 ? 'up' : 'down',
		...modifiers,
	};
}

function decodeModifiers(
	buttonCode: number,
): Pick<WheelMouseEvent, 'ctrl' | 'shift' | 'alt'> {
	return {
		ctrl: (buttonCode & 16) !== 0,
		shift: (buttonCode & 4) !== 0,
		alt: (buttonCode & 8) !== 0,
	};
}

class DelegatingReadable extends Readable {
	private rawMode = false;

	constructor(private readonly source: StdinLike) {
		super();
	}

	get isTTY(): boolean {
		return Boolean(this.source.isTTY);
	}

	get isRaw(): boolean {
		return this.source.isRaw ?? this.rawMode;
	}

	setRawMode(enabled: boolean): this {
		this.source.setRawMode?.(enabled);
		this.rawMode = enabled;
		return this;
	}

	ref(): this {
		this.source.ref?.();
		return this;
	}

	unref(): this {
		this.source.unref?.();
		return this;
	}

	_read(): void {}
}
