import {describe, expect, test} from 'bun:test';
import React from 'react';
import {render} from 'ink-testing-library';
import {HistoryRegion} from './HistoryRegion.js';
import type {HistoryMessage} from './types.js';

describe('HistoryRegion text rendering', () => {
	test('keeps user, assistant, and system messages visible', () => {
		const messages: HistoryMessage[] = [
			{id: 'user-1', role: 'user', text: 'hello'},
			{id: 'assistant-1', role: 'assistant', text: 'hi there'},
			{id: 'system-1', role: 'system', text: 'Model set to openai/gpt-4.1'},
		];

		const {lastFrame} = render(
			<HistoryRegion messages={messages} height={8} />,
		);
		const frame = lastFrame() ?? '';

		expect(frame).toContain('hello');
		expect(frame).toContain('hi there');
		expect(frame).toContain('Model set to openai/gpt-4.1');
	});
});
