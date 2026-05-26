import {describe, expect, test} from 'bun:test';
import React from 'react';
import {render} from 'ink-testing-library';
import {COMPOSER_HEIGHT, ComposerRegion} from './ComposerRegion.js';
import {
	COMPOSER_ROW_COUNT,
	assertComposerLayout,
	horizontalRule,
} from './layout.js';

const WIDTH = 40;
const HINT = 'Enter send · /model · /exit';

function renderComposer(
	overrides: Partial<React.ComponentProps<typeof ComposerRegion>> = {},
) {
	return render(
		<ComposerRegion
			value=""
			busy={false}
			hint={HINT}
			width={WIDTH}
			focused
			onChange={() => {}}
			onSubmit={() => {}}
			{...overrides}
		/>,
	);
}

describe('ComposerRegion layout invariants', () => {
	test('reserves four terminal rows', () => {
		expect(COMPOSER_HEIGHT).toBe(COMPOSER_ROW_COUNT);
	});

	test('empty focused: rules, prompt row, hint (cursor may be invisible in plain frames)', () => {
		const {lastFrame} = renderComposer({value: '', focused: true});
		assertComposerLayout(lastFrame(), {width: WIDTH, hint: HINT});
		const inputLine = lastFrame()?.split('\n')[1] ?? '';
		expect(inputLine === '>' || inputLine.startsWith('> ')).toBe(true);
	});

	test('with text: prompt and value on one line', () => {
		const {lastFrame} = renderComposer({
			value: 'hello',
			focused: false,
		});
		assertComposerLayout(lastFrame(), {
			width: WIDTH,
			hint: HINT,
			inputLine: '> hello',
		});
	});

	test('busy: prompt, value, and Running… hint', () => {
		const {lastFrame} = renderComposer({
			value: 'working',
			busy: true,
		});
		assertComposerLayout(lastFrame(), {
			width: WIDTH,
			hint: 'Running…',
			inputLine: '> working',
		});
	});

	test('horizontal rules span the configured width', () => {
		expect(horizontalRule(WIDTH)).toBe('─'.repeat(WIDTH));
		expect(horizontalRule(WIDTH).length).toBe(WIDTH);
	});
});

describe('ComposerRegion golden frames', () => {
	test('with text (stable plain-text frame)', () => {
		const {lastFrame} = renderComposer({
			value: 'hello',
			focused: false,
		});
		expect(lastFrame()).toMatchSnapshot();
	});

	test('busy (stable plain-text frame)', () => {
		const {lastFrame} = renderComposer({
			value: 'hello',
			busy: true,
		});
		expect(lastFrame()).toMatchSnapshot();
	});
});

describe('ComposerRegion regression guards', () => {
	test('does not use round prompt border (Claude uses horizontal rules)', () => {
		const {lastFrame} = renderComposer({value: 'x', focused: false});
		const frame = lastFrame() ?? '';
		expect(frame).not.toContain('╭');
		expect(frame).not.toContain('╰');
		expect(frame).not.toContain('Ask anything');
	});

	test('input line uses > prompt prefix when showing text', () => {
		const {lastFrame} = renderComposer({
			value: 'typed',
			focused: false,
		});
		expect(lastFrame()?.split('\n')[1]).toBe('> typed');
	});
});
