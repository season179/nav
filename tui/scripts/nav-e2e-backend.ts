#!/usr/bin/env bun
import http, {type IncomingMessage, type ServerResponse} from 'node:http';

export const E2E_COMMANDS = [
	'pwd',
	'ls tui/src',
	'rg VirtualHistoryRegion tui/src',
	'bun test',
	'bun run typecheck',
	'git status --short',
];

export const E2E_FINAL_TEXT = 'Final assistant message: residue check complete.';
export const E2E_WHEEL_REVEALED_TEXT =
	'Earlier context 01: user asked about #374 residue.';

export type E2eProtocolEvent = {
	id: string;
	type: string;
	payload: Record<string, unknown>;
};

type JsonRpcRequest = {
	jsonrpc?: string;
	id?: string | number | null;
	method?: string;
	params?: Record<string, unknown>;
};

type SessionState = {
	events: E2eProtocolEvent[];
};

const sessions = new Map<string, SessionState>();

export function createE2eRunEvents(
	sessionId: string,
	runId: string,
): E2eProtocolEvent[] {
	const messageId = `${runId}-assistant`;
	const events: E2eProtocolEvent[] = [
		event(`${runId}-001`, 'run.started', {
			session_id: sessionId,
			run_id: runId,
		}),
	];

	for (let index = 1; index <= 12; index += 1) {
		const number = String(index).padStart(2, '0');
		events.push(
			event(`${runId}-context-${number}`, 'file.changed', {
				session_id: sessionId,
				file_change_id: `${runId}-context-${number}`,
				path: `Earlier context ${number}: user asked about #374 residue.`,
				kind: 'modified',
			}),
		);
	}

	for (const [index, command] of E2E_COMMANDS.entries()) {
		const number = index + 1;
		const toolCallId = `${runId}-bash-${number}`;
		const prefix = String(number + 1).padStart(3, '0');
		events.push(
			event(`${runId}-${prefix}a`, 'tool.call_started', {
				session_id: sessionId,
				run_id: runId,
				tool_call_id: toolCallId,
				name: 'bash',
			}),
			event(`${runId}-${prefix}b`, 'tool.call_delta', {
				session_id: sessionId,
				run_id: runId,
				tool_call_id: toolCallId,
				arguments_delta: JSON.stringify({command}),
			}),
			event(`${runId}-${prefix}c`, 'tool.output_delta', {
				session_id: sessionId,
				run_id: runId,
				tool_call_id: toolCallId,
				stream: 'stdout',
				chunk: `running ${number}\n`,
			}),
			event(`${runId}-${prefix}d`, 'tool.call_completed', {
				session_id: sessionId,
				run_id: runId,
				tool_call_id: toolCallId,
				name: 'bash',
				arguments: JSON.stringify({command}),
				output: `command ${number} completed\n`,
				output_lossy: false,
			}),
		);
	}

	events.push(
		event(`${runId}-090`, 'model.text_delta', {
			session_id: sessionId,
			run_id: runId,
			message_id: messageId,
			delta: E2E_FINAL_TEXT,
		}),
		event(`${runId}-091`, 'message.completed', {
			session_id: sessionId,
			run_id: runId,
			message_id: messageId,
			finish_reason: 'stop',
		}),
		event(`${runId}-092`, 'run.completed', {
			session_id: sessionId,
			run_id: runId,
		}),
	);

	return events;
}

function event(
	id: string,
	type: string,
	payload: Record<string, unknown>,
): E2eProtocolEvent {
	return {
		id,
		type,
		payload,
	};
}

function startServer(): void {
	const server = http.createServer((request, response) => {
		void handleRequest(request, response);
	});

	server.listen(0, '127.0.0.1', () => {
		const address = server.address();
		if (!address || typeof address === 'string') {
			throw new Error('could not bind e2e backend');
		}
		console.log(
			JSON.stringify({
				type: 'backend.ready',
				baseUrl: `http://127.0.0.1:${address.port}`,
			}),
		);
	});

	const close = (): void => {
		server.close();
	};
	process.once('SIGINT', close);
	process.once('SIGTERM', close);
}

async function handleRequest(
	request: IncomingMessage,
	response: ServerResponse,
): Promise<void> {
	const url = new URL(request.url ?? '/', 'http://127.0.0.1');

	if (request.method === 'POST' && url.pathname === '/rpc') {
		await handleRpc(request, response);
		return;
	}

	const eventMatch = url.pathname.match(/^\/sessions\/([^/]+)\/events$/);
	if (request.method === 'GET' && eventMatch) {
		handleEvents(request, response, eventMatch[1]!);
		return;
	}

	writeJson(response, 404, {error: 'not found'});
}

async function handleRpc(
	request: IncomingMessage,
	response: ServerResponse,
): Promise<void> {
	const rpc = JSON.parse(await readRequestBody(request)) as JsonRpcRequest;

	if (rpc.method === 'session.create') {
		const sessionId = `session-${Date.now()}`;
		sessions.set(sessionId, {
			events: [
				event('session-created', 'session.created', {
					session_id: sessionId,
				}),
			],
		});
		writeRpcResult(response, rpc.id, {sessionId});
		return;
	}

	if (rpc.method === 'session.sendMessage') {
		const sessionId = String(rpc.params?.sessionId ?? '');
		const session = sessions.get(sessionId);
		if (!session) {
			writeRpcError(response, rpc.id, -32001, 'unknown session');
			return;
		}

		const runId = `run-${Date.now()}`;
		session.events.push(...createE2eRunEvents(sessionId, runId));
		writeRpcResult(response, rpc.id, {runId});
		return;
	}

	writeRpcError(response, rpc.id, -32601, `unknown method: ${rpc.method ?? ''}`);
}

function handleEvents(
	request: IncomingMessage,
	response: ServerResponse,
	sessionId: string,
): void {
	const session = sessions.get(sessionId);
	if (!session) {
		response.writeHead(404, {'Content-Type': 'text/plain'});
		response.end('unknown session');
		return;
	}

	const lastEventId = request.headers['last-event-id'];
	const previousId = Array.isArray(lastEventId) ? lastEventId[0] : lastEventId;
	const startIndex = previousId
		? session.events.findIndex(item => item.id === previousId) + 1
		: 0;
	const events = session.events.slice(Math.max(0, startIndex));

	response.writeHead(200, {
		'Content-Type': 'text/event-stream',
		'Cache-Control': 'no-cache',
		Connection: 'close',
	});
	for (const item of events) {
		response.write(encodeSse(item));
	}
	response.end();
}

function encodeSse(item: E2eProtocolEvent): string {
	return [
		`id: ${item.id}`,
		`event: ${item.type}`,
		`data: ${JSON.stringify({type: item.type, ...item.payload})}`,
		'',
		'',
	].join('\n');
}

function writeRpcResult(
	response: ServerResponse,
	id: JsonRpcRequest['id'],
	result: unknown,
): void {
	writeJson(response, 200, {
		jsonrpc: '2.0',
		id,
		result,
	});
}

function writeRpcError(
	response: ServerResponse,
	id: JsonRpcRequest['id'],
	code: number,
	message: string,
): void {
	writeJson(response, 200, {
		jsonrpc: '2.0',
		id,
		error: {code, message},
	});
}

function writeJson(
	response: ServerResponse,
	status: number,
	body: unknown,
): void {
	response.writeHead(status, {'Content-Type': 'application/json'});
	response.end(JSON.stringify(body));
}

function readRequestBody(request: IncomingMessage): Promise<string> {
	return new Promise((resolve, reject) => {
		const chunks: Buffer[] = [];
		request.on('data', chunk => {
			chunks.push(Buffer.from(chunk));
		});
		request.on('end', () => {
			resolve(Buffer.concat(chunks).toString('utf8'));
		});
		request.on('error', reject);
	});
}

if (import.meta.main) {
	switch (process.argv[2]) {
		case 'serve-http':
			startServer();
			break;
		case 'print-commands':
			console.log(E2E_COMMANDS.join('\n'));
			break;
		case 'print-final-text':
			console.log(E2E_FINAL_TEXT);
			break;
		case 'print-wheel-revealed':
			console.log(E2E_WHEEL_REVEALED_TEXT);
			break;
		default:
			console.error(
				'usage: nav-e2e-backend.ts serve-http|print-commands|print-final-text|print-wheel-revealed',
			);
			process.exit(2);
	}
}
