import {describe, expect, test} from 'bun:test';
import {readFileSync} from 'node:fs';
import path from 'node:path';
import {parseSse, readSseStream, type NavEvent} from './client.js';

describe('parseSse', () => {
	test('parses a tool call started payload from SSE', () => {
		const events: NavEvent[] = [];

		parseSse(
			[
				'id: 019f2f6f-f178-7a72-9f28-000000000302',
				'event: tool.call_started',
				'data: {"event_id":"019f2f6f-f178-7a72-9f28-000000000302","session_id":"019f2f6f-f178-7a72-9f28-000000000100","type":"tool.call_started","run_id":"019f2f6f-f178-7a72-9f28-000000000301","tool_call_id":"019f2f6f-f178-7a72-9f28-000000000303","name":"read"}',
				'',
				'',
			].join('\n'),
			event => {
				events.push(event);
				return false;
			},
		);

		expect(events).toEqual([
			expect.objectContaining({
				id: '019f2f6f-f178-7a72-9f28-000000000302',
				type: 'tool.call_started',
				sessionId: '019f2f6f-f178-7a72-9f28-000000000100',
				runId: '019f2f6f-f178-7a72-9f28-000000000301',
				toolCallId: '019f2f6f-f178-7a72-9f28-000000000303',
				name: 'read',
			}),
		]);
	});
});

describe('readSseStream', () => {
	test('parses tool call payloads from recorded SSE bytes', async () => {
		const readEvents = await collectEvents(
			protocolFixture('tool-call-read.sse'),
		);
		const failedEvents = await collectEvents(
			protocolFixture('tool-call-failed.sse'),
		);

		const started = readEvents.find(
			event => event.type === 'tool.call_started',
		);
		const delta = readEvents.find(event => event.type === 'tool.call_delta');
		const completed = readEvents.find(
			event => event.type === 'tool.call_completed',
		);
		const failed = failedEvents.find(
			event => event.type === 'tool.call_failed',
		);

		expect(started).toMatchObject({
			toolCallId: '019f2f6f-f178-7a72-9f28-000000000303',
			name: 'read',
		});
		expect(delta).toMatchObject({
			toolCallId: '019f2f6f-f178-7a72-9f28-000000000303',
			argumentsDelta: '{"path":"fixture.txt"}',
		});
		expect(completed).toMatchObject({
			toolCallId: '019f2f6f-f178-7a72-9f28-000000000303',
			name: 'read',
			arguments: '{"path":"fixture.txt"}',
		});
		expect(failed).toMatchObject({
			toolCallId: '019f2f6f-f178-7a72-9f28-000000000403',
			name: 'read',
			errorMessage: 'path escapes workspace',
		});
	});

	test('parses approval and file payloads from SSE bytes', async () => {
		const events = await collectEvents(
			[
				sse('tool.approval_requested', {
					event_id: 'evt-approval',
					session_id: 'session-1',
					type: 'tool.approval_requested',
					run_id: 'run-1',
					tool_call_id: 'tool-3',
					approval_id: 'approval-1',
				}),
				sse('file.changed', {
					event_id: 'evt-file',
					session_id: 'session-1',
					type: 'file.changed',
					file_change_id: 'change-1',
					path: 'src/app.ts',
				}),
			].join(''),
		);

		expect(events.map(event => event.type)).toEqual([
			'tool.approval_requested',
			'file.changed',
		]);
		expect(events[0]).toMatchObject({
			toolCallId: 'tool-3',
			approvalId: 'approval-1',
		});
		expect(events[1]).toMatchObject({
			fileChangeId: 'change-1',
			path: 'src/app.ts',
		});
	});
});

function protocolFixture(name: string): string {
	return readFileSync(
		path.join(
			import.meta.dir,
			'..',
			'..',
			'..',
			'fixtures',
			'protocol',
			'event-streams',
			name,
		),
		'utf8',
	);
}

async function collectEvents(input: string): Promise<NavEvent[]> {
	const events: NavEvent[] = [];
	for await (const event of readSseStream(byteStream(input), () => {})) {
		events.push(event);
	}
	return events;
}

function byteStream(input: string): ReadableStream<Uint8Array> {
	const encoder = new TextEncoder();
	const midpoint = Math.floor(input.length / 2);
	const chunks = [
		encoder.encode(input.slice(0, midpoint)),
		encoder.encode(input.slice(midpoint)),
	];

	return new ReadableStream({
		start(controller) {
			for (const chunk of chunks) {
				controller.enqueue(chunk);
			}
			controller.close();
		},
	});
}

function sse(event: string, payload: Record<string, unknown>): string {
	return [
		`id: ${payload.event_id}`,
		`event: ${event}`,
		`data: ${JSON.stringify(payload)}`,
		'',
		'',
	].join('\n');
}
