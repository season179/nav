import {describe, expect, test} from 'bun:test';
import {
	createE2eRunEvents,
	E2E_COMMANDS,
	E2E_FINAL_TEXT,
	E2E_WHEEL_REVEALED_TEXT,
} from './nav-e2e-backend.js';

describe('nav E2E backend fixture', () => {
	test('emits nav-shaped events for the tmux residue smoke', () => {
		const events = createE2eRunEvents('session-e2e', 'run-e2e');

		expect(E2E_COMMANDS).toHaveLength(6);
		expect(E2E_WHEEL_REVEALED_TEXT).toContain('Earlier context 01');
		expect(events.at(0)?.type).toBe('run.started');
		expect(events.at(-1)?.type).toBe('run.completed');
		expect(
			events.some(
				event =>
					event.type === 'model.text_delta' &&
					event.payload.delta === E2E_FINAL_TEXT,
			),
		).toBe(true);

		const completedCalls = events.filter(
			event => event.type === 'tool.call_completed',
		);
		expect(completedCalls).toHaveLength(E2E_COMMANDS.length);
		expect(completedCalls.map(event => event.payload.arguments)).toEqual(
			E2E_COMMANDS.map(command => JSON.stringify({command})),
		);
	});

	test('uses unique event ids across repeated runs', () => {
		const firstRun = createE2eRunEvents('session-e2e', 'run-one');
		const secondRun = createE2eRunEvents('session-e2e', 'run-two');
		const ids = [...firstRun, ...secondRun].map(event => event.id);

		expect(new Set(ids).size).toBe(ids.length);
	});
});
