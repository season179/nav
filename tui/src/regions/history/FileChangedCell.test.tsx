import {describe, expect, test} from 'bun:test';
import React from 'react';
import {render} from 'ink-testing-library';
import {FileChangedCell} from './FileChangedCell.js';
import type {FileChangedHistoryMessage} from './types.js';

describe('FileChangedCell snapshots', () => {
	test('modified change shows the kind and path on one line', () => {
		const frame = render(
			<FileChangedCell message={fileChanged({kind: 'modified'})} />,
		).lastFrame();

		expect(frame).toContain('modified');
		expect(frame).toContain('src/app.ts');
		expect(frameLineCount(frame)).toBe(1);
		expect(frame).toMatchSnapshot();
	});

	test('created change reads as created', () => {
		const frame = render(
			<FileChangedCell message={fileChanged({kind: 'created'})} />,
		).lastFrame();

		expect(frame).toContain('created');
		expect(frame).toMatchSnapshot();
	});

	test('deleted change reads as deleted', () => {
		const frame = render(
			<FileChangedCell message={fileChanged({kind: 'deleted'})} />,
		).lastFrame();

		expect(frame).toContain('deleted');
		expect(frame).toMatchSnapshot();
	});

	test('missing kind still renders the path without a label', () => {
		const frame = render(
			<FileChangedCell message={fileChanged({kind: undefined})} />,
		).lastFrame();

		expect(frame).toContain('src/app.ts');
		expect(frameLineCount(frame)).toBe(1);
		expect(frame).toMatchSnapshot();
	});

	test('empty path renders an (unknown) placeholder rather than a blank chip', () => {
		const frame = render(
			<FileChangedCell message={fileChanged({path: ''})} />,
		).lastFrame();

		expect(frame).toContain('(unknown)');
		expect(frameLineCount(frame)).toBe(1);
		expect(frame).toMatchSnapshot();
	});
});

function fileChanged(
	overrides: Partial<FileChangedHistoryMessage> = {},
): FileChangedHistoryMessage {
	return {
		id: 'change-1',
		role: 'file_changed',
		path: 'src/app.ts',
		kind: 'modified',
		...overrides,
	};
}

function frameLineCount(frame: string | undefined): number {
	return frame?.split('\n').filter(line => line.length > 0).length ?? 0;
}
