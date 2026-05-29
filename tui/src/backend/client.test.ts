import {describe, expect, test} from 'bun:test';
import {readFileSync} from 'node:fs';
import path from 'node:path';
import {
	NavBackendClient,
	ApprovalError,
	parseSse,
	readSseStream,
	type NavEvent,
} from './client.js';

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

	test('parses legacy message delta payloads', () => {
		const events: NavEvent[] = [];

		parseSse(
			[
				'id: evt-message-delta',
				'event: message.delta',
				'data: {"event_id":"evt-message-delta","session_id":"session-1","type":"message.delta","run_id":"run-1","message_id":"message-1","text":"legacy text"}',
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
				type: 'message.delta',
				runId: 'run-1',
				messageId: 'message-1',
				text: 'legacy text',
			}),
		]);
	});

	test('parses session.totals_updated payload from SSE', () => {
		const events: NavEvent[] = [];

		parseSse(
			[
				'id: evt-totals',
				'event: session.totals_updated',
				'data: {"event_id":"evt-totals","session_id":"session-1","type":"session.totals_updated","cost":0.0523,"tokens_input":1500,"tokens_output":800,"tokens_reasoning":0,"tokens_cache_read":200,"tokens_cache_write":100}',
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
				type: 'session.totals_updated',
				sessionId: 'session-1',
				cost: 0.0523,
				tokensInput: 1500,
				tokensOutput: 800,
				tokensReasoning: 0,
				tokensCacheRead: 200,
				tokensCacheWrite: 100,
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
					tool_name: 'bash',
					reason: 'bash requires approval',
					arguments_summary: '{"cmd":"echo hi"}',
					risk_class: 'exec',
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
			toolName: 'bash',
			reason: 'bash requires approval',
			argumentsSummary: '{"cmd":"echo hi"}',
			riskClass: 'exec',
		});
		expect(events[1]).toMatchObject({
			fileChangeId: 'change-1',
			path: 'src/app.ts',
		});
	});

	test('parses bash output deltas and final output fields', async () => {
		const events = await collectEvents(
			[
				sse('tool.output_delta', {
					event_id: 'evt-output',
					session_id: 'session-1',
					type: 'tool.output_delta',
					run_id: 'run-1',
					tool_call_id: 'tool-1',
					stream: 'stdout',
					chunk: 'first\n',
				}),
				sse('tool.call_completed', {
					event_id: 'evt-completed',
					session_id: 'session-1',
					type: 'tool.call_completed',
					run_id: 'run-1',
					tool_call_id: 'tool-1',
					name: 'bash',
					arguments: '{"command":"printf first"}',
					output: 'first\n',
					output_lossy: false,
				}),
				sse('tool.call_failed', {
					event_id: 'evt-failed',
					session_id: 'session-1',
					type: 'tool.call_failed',
					run_id: 'run-1',
					tool_call_id: 'tool-1',
					name: 'bash',
					error_message: 'command exited with status 7',
					output: 'partial\n',
					output_lossy: true,
				}),
			].join(''),
		);

		expect(events[0]).toMatchObject({
			type: 'tool.output_delta',
			toolCallId: 'tool-1',
			stream: 'stdout',
			chunk: 'first\n',
		});
		expect(events[1]).toMatchObject({
			type: 'tool.call_completed',
			toolCallId: 'tool-1',
			name: 'bash',
			arguments: '{"command":"printf first"}',
			output: 'first\n',
			outputLossy: false,
		});
		expect(events[2]).toMatchObject({
			type: 'tool.call_failed',
			toolCallId: 'tool-1',
			name: 'bash',
			errorMessage: 'command exited with status 7',
			output: 'partial\n',
			outputLossy: true,
		});
	});

	test('parses part deltas and completion from recorded SSE bytes', async () => {
		const events = await collectEvents(protocolFixture('part-delta.sse'));

		expect(events.map(event => event.type)).toEqual([
			'session.created',
			'part.delta',
			'part.delta',
			'part.delta',
			'part.completed',
		]);
		expect(events[1]).toMatchObject({
			type: 'part.delta',
			turnId: '019f2f6f-f178-7a72-9f28-000000000102',
			partId: 'prt_0000018bcfe56800_0000000000000001',
			field: 'text',
			delta: 'hello',
		});
		expect(events[3]).toMatchObject({
			type: 'part.delta',
			partId: 'prt_0000018bcfe56800_0000000000000002',
			field: 'arguments',
			delta: '{"path":"fixture.txt"}',
		});
		expect(events[4]).toMatchObject({
			type: 'part.completed',
			partId: 'prt_0000018bcfe56800_0000000000000001',
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

// --- Fake transport for NavBackendClient RPC tests ---

type CapturedRequest = {
	jsonrpc: string;
	id: string;
	method: string;
	params?: unknown;
};

type FakeResponse = {
	result?: unknown;
	error?: {code: number; message: string};
};

function createClientWithFakeRpc(responses: FakeResponse[]) {
	const client = new NavBackendClient();
	const requests: CapturedRequest[] = [];
	let callIndex = 0;

	// Bypass backend spawning — wire in a fake endpoint and transport.
	(client as any).endpoint = 'http://fake';
	(client as any).session = {
		sessionId: 'test-session',
		endpoint: 'http://fake',
		cwd: '/tmp',
	};
	(client as any).fetchImpl = async (
		_url: string,
		options: {body: string},
	) => {
		const request = JSON.parse(options.body) as CapturedRequest;
		requests.push(request);
		const response = responses[callIndex++] as FakeResponse;

		const body = JSON.stringify({
			jsonrpc: '2.0',
			id: request.id,
			...(response.error
				? {error: response.error}
				: {result: response.result}),
		});

		return {
			ok: true,
			status: 200,
			text: async () => body,
		};
	};

	return {client, getRequests: () => requests};
}

// --- NavBackendClient public methods ---

describe('NavBackendClient public methods', () => {
	test('streamMessage passes the abort signal to the events fetch', async () => {
		const client = new NavBackendClient();
		const controller = new AbortController();
		let eventFetchSignal: AbortSignal | undefined;
		(client as any).endpoint = 'http://fake';
		(client as any).session = {
			sessionId: 'test-session',
			endpoint: 'http://fake',
			cwd: '/tmp',
		};
		(client as any).fetchImpl = async (
			url: string,
			options: RequestInit = {},
		) => {
			if (url.endsWith('/rpc')) {
				return jsonResponse({result: {runId: 'run-1'}});
			}

			eventFetchSignal = options.signal ?? undefined;
			return {
				ok: true,
				status: 200,
				text: async () => '',
				body: byteStream(
					sse('run.completed', {
						event_id: 'evt-run-completed',
						session_id: 'test-session',
						type: 'run.completed',
						run_id: 'run-1',
					}),
				),
			};
		};

		const events: NavEvent[] = [];
		for await (const event of client.streamMessage('hello', {
			signal: controller.signal,
		})) {
			events.push(event);
		}

		expect(eventFetchSignal).toBe(controller.signal);
		expect(events.at(-1)).toMatchObject({
			type: 'run.completed',
			runId: 'run-1',
		});
	});

	test('approveTool sends tool.approve RPC and returns parsed result', async () => {
		const {client, getRequests} = createClientWithFakeRpc([
			{result: {approval_id: 'appr-1', outcome: 'approved'}},
		]);

		const result = await client.approveTool('appr-1');

		expect(result).toEqual({approvalId: 'appr-1', outcome: 'approved'});
		expect(getRequests()[0]).toMatchObject({
			method: 'tool.approve',
			params: {approval_id: 'appr-1'},
		});
		expectUuidV7(getRequests()[0].id);
	});

	test('rejectTool sends tool.reject RPC with optional reason and returns result', async () => {
		const {client, getRequests} = createClientWithFakeRpc([
			{result: {approval_id: 'appr-2', outcome: 'rejected'}},
		]);

		const result = await client.rejectTool('appr-2', 'not this command');

		expect(result).toEqual({approvalId: 'appr-2', outcome: 'rejected'});
		expect(getRequests()[0]).toMatchObject({
			method: 'tool.reject',
			params: {
				approval_id: 'appr-2',
				reason: 'not this command',
			},
		});
	});

	test('rejectTool omits reason when not provided', async () => {
		const {client, getRequests} = createClientWithFakeRpc([
			{result: {approval_id: 'appr-3', outcome: 'rejected'}},
		]);

		await client.rejectTool('appr-3');

		expect(getRequests()[0].params).toEqual({approval_id: 'appr-3'});
	});

	test('approveTool throws ApprovalError not_pending for unknown approval id', async () => {
		const {client} = createClientWithFakeRpc([
			{
				error: {
					code: -32006,
					message: 'approval `unknown-1` is not pending',
				},
			},
		]);

		try {
			await client.approveTool('unknown-1');
			expect.unreachable('should have thrown');
		} catch (error) {
			expect(error).toBeInstanceOf(ApprovalError);
			expect((error as ApprovalError).kind).toBe('not_pending');
			expect((error as ApprovalError).message).toContain('not pending');
		}
	});

	test('rejectTool throws ApprovalError not_pending for already-resolved approval id', async () => {
		const {client} = createClientWithFakeRpc([
			{
				error: {
					code: -32006,
					message: 'approval `appr-done` is already pending',
				},
			},
		]);

		try {
			await client.rejectTool('appr-done');
			expect.unreachable('should have thrown');
		} catch (error) {
			expect(error).toBeInstanceOf(ApprovalError);
			expect((error as ApprovalError).kind).toBe('not_pending');
		}
	});

	test('approveTool throws ApprovalError network for non-32006 RPC errors', async () => {
		const {client} = createClientWithFakeRpc([
			{
				error: {
					code: -32600,
					message: 'invalid request',
				},
			},
		]);

		try {
			await client.approveTool('appr-1');
			expect.unreachable('should have thrown');
		} catch (error) {
			expect(error).toBeInstanceOf(ApprovalError);
			expect((error as ApprovalError).kind).toBe('network');
		}
	});

	test('approveTool throws ApprovalError network when fetch throws', async () => {
		const client = new NavBackendClient();
		(client as any).endpoint = 'http://fake';
		(client as any).session = {
			sessionId: 'test-session',
			endpoint: 'http://fake',
			cwd: '/tmp',
		};
		(client as any).fetchImpl = async () => {
			throw new TypeError('fetch failed');
		};

		try {
			await client.approveTool('appr-net');
			expect.unreachable('should have thrown');
		} catch (error) {
			expect(error).toBeInstanceOf(ApprovalError);
			expect((error as ApprovalError).kind).toBe('network');
			expect((error as ApprovalError).message).toContain('fetch failed');
		}
	});

	test('approveTool throws ApprovalError network for unexpected result shapes', async () => {
		const {client} = createClientWithFakeRpc([
			{result: {approval_id: 'appr-odd', outcome: 'maybe'}},
		]);

		try {
			await client.approveTool('appr-odd');
			expect.unreachable('should have thrown');
		} catch (error) {
			expect(error).toBeInstanceOf(ApprovalError);
			expect((error as ApprovalError).kind).toBe('network');
			expect((error as ApprovalError).message).toContain(
				'unexpected result shape',
			);
		}
	});
});

function jsonResponse(response: FakeResponse): Response {
	return {
		ok: true,
		status: 200,
		text: async () =>
			JSON.stringify({
				jsonrpc: '2.0',
				id: 'response-id',
				...(response.error ? {error: response.error} : {result: response.result}),
			}),
		} as Response;
}

function expectUuidV7(value: string): void {
	expect(value).toMatch(
		/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
	);
}
