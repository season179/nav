import {spawn, type ChildProcess} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {createInterface} from 'node:readline';
import {access, constants, readFile} from 'node:fs/promises';
import path from 'node:path';

export type ToolsPreset = 'coding' | 'readonly';

export type FileChangeKind = 'created' | 'modified' | 'deleted';

const RPC_SESSION_CREATE = 'session.create';
const RPC_SESSION_SEND_MESSAGE = 'session.sendMessage';

type SessionCreateParams = {
	cwd?: string;
	source?: string;
	toolsPreset?: ToolsPreset;
};

type NavEventBase = {
	id: string;
	sessionId: string;
};

type RunScopedEvent = NavEventBase & {
	runId: string;
};

type MessageScopedEvent = RunScopedEvent & {
	messageId: string;
};

type PartScopedEvent = NavEventBase & {
	turnId: string;
	partId: string;
};

type ToolScopedEvent = RunScopedEvent & {
	toolCallId: string;
};

export type NavEvent =
	| (NavEventBase & {type: 'session.created'})
	| (RunScopedEvent & {
			type: 'run.started' | 'run.completed' | 'run.cancelled';
	  })
	| (MessageScopedEvent & {
			type: 'model.text_delta' | 'model.reasoning_delta';
			delta: string;
	  })
	| (MessageScopedEvent & {
			type: 'message.delta';
			text: string;
	  })
	| (PartScopedEvent & {
			type: 'part.delta';
			field: string;
			delta: string;
	  })
	| (PartScopedEvent & {
			type: 'part.completed';
	  })
	| (MessageScopedEvent & {
			type: 'message.completed';
			finishReason: string;
	  })
	| (ToolScopedEvent & {
			type: 'tool.call_requested';
			name: string;
	  })
	| (ToolScopedEvent & {
			type: 'tool.call_started';
			name: string;
	  })
	| (ToolScopedEvent & {
			type: 'tool.call_delta';
			argumentsDelta: string;
	  })
	| (ToolScopedEvent & {
			type: 'tool.output_delta';
			stream: 'stdout' | 'stderr' | string;
			chunk: string;
	  })
	| (ToolScopedEvent & {
			type: 'tool.call_completed';
			name: string;
			arguments: string;
			output?: string;
			outputLossy?: boolean;
	  })
	| (ToolScopedEvent & {
			type: 'tool.call_failed';
			name: string;
			errorMessage: string;
			output?: string;
			outputLossy?: boolean;
	  })
	| (ToolScopedEvent & {
			type: 'tool.approval_requested';
			approvalId: string;
			toolName: string;
			reason: string;
			argumentsSummary: string;
			riskClass?: string;
	  })
	| (NavEventBase & {
			type: 'file.changed';
			fileChangeId: string;
			path: string;
			kind?: FileChangeKind;
	  })
	| (RunScopedEvent & {type: 'run.failed'; message: string})
	| (RunScopedEvent & {
			type: 'provider.error';
			message: string;
			status?: number;
			errorType?: string;
			code?: string;
	  })
	| (NavEventBase & {type: 'error'; message: string})
	| (NavEventBase & {
			type: 'unknown';
			rawType: string;
			payload: Record<string, unknown>;
	  });

type SseEventFrame = {
	id: string;
	type: string;
};

export type SessionInfo = {
	sessionId: string;
	endpoint: string;
	cwd: string;
};

type JsonRpcRequest = {
	jsonrpc: '2.0';
	id: string;
	method: string;
	params?: unknown;
};

type JsonRpcResponse = {
	jsonrpc: '2.0';
	id: string;
	result?: unknown;
	error?: {code: number; message: string};
};

type ApprovalRpcResult = {
	approvalId?: string;
	approval_id?: string;
	outcome?: string;
};

type BackendReady = {
	type: string;
	baseUrl: string;
};

type EventPayload = {
	event_id?: string;
	session_id?: string;
	type?: string;
	run_id?: string;
	message_id?: string;
	turn_id?: string;
	part_id?: string;
	field?: string;
	finish_reason?: string;
	tool_call_id?: string;
	name?: string;
	arguments_delta?: string;
	arguments?: string;
	stream?: string;
	chunk?: string;
	output?: string;
	output_lossy?: boolean;
	error_message?: string;
	approval_id?: string;
	tool_name?: string;
	reason?: string;
	arguments_summary?: string;
	risk_class?: string;
	file_change_id?: string;
	change_id?: string;
	path?: string;
	kind?: string;
	status?: number;
	error_type?: string;
	code?: string;
	delta?: string;
	text?: string;
	message?: string;
};

const RPC_TOOL_APPROVE = 'tool.approve';
const RPC_TOOL_REJECT = 'tool.reject';

export type ApprovalResult = {
	approvalId: string;
	outcome: 'approved' | 'rejected';
};

export type StreamMessageOptions = {
	signal: AbortSignal;
};

export class RpcError extends Error {
	constructor(
		public readonly code: number,
		message: string,
	) {
		super(message);
		this.name = 'RpcError';
	}
}

export class ApprovalError extends Error {
	constructor(
		public readonly kind: 'not_pending' | 'network',
		message: string,
	) {
		super(message);
		this.name = 'ApprovalError';
	}
}

const APPROVAL_NOT_PENDING_CODE = -32006;

function toApprovalError(error: unknown): ApprovalError {
	if (error instanceof RpcError) {
		const kind = error.code === APPROVAL_NOT_PENDING_CODE ? 'not_pending' : 'network';
		return new ApprovalError(kind, error.message);
	}
	if (error instanceof Error) {
		return new ApprovalError('network', error.message);
	}
	return new ApprovalError('network', String(error));
}

export class NavBackendClient {
	private backendPath: string;
	private endpoint = '';
	private child: ChildProcess | null = null;
	private session: SessionInfo | null = null;
	private lastEventId = '';
	private fetchImpl: typeof fetch;

	constructor(backendPath = '') {
		this.backendPath = backendPath;
		this.fetchImpl = globalThis.fetch.bind(globalThis);
	}

	async connect(toolsPreset?: ToolsPreset): Promise<SessionInfo> {
		if (this.session) {
			return this.session;
		}

		await this.startBackend();
		const cwd = process.cwd();
		const params: SessionCreateParams = {
			cwd,
			source: 'tui',
			toolsPreset,
		};
		const result = await this.callRpc(RPC_SESSION_CREATE, params);
		const create = result as {sessionId?: string};
		if (!create.sessionId) {
			throw new Error('session.create returned an empty session id');
		}

		this.session = {
			sessionId: create.sessionId,
			endpoint: this.endpoint,
			cwd,
		};

		await this.fetchEvents(event => event.type === 'session.created');
		return this.session;
	}

	async *streamMessage(
		text: string,
		options: StreamMessageOptions,
	): AsyncGenerator<NavEvent, void, void> {
		await this.connect();
		const trimmed = text.trim();
		if (!trimmed) {
			throw new Error('message text is required');
		}

		const result = await this.callRpc(RPC_SESSION_SEND_MESSAGE, {
			sessionId: this.session!.sessionId,
			text: trimmed,
		});
		const send = result as {runId?: string};
		if (!send.runId) {
			throw new Error('session.sendMessage returned an empty run id');
		}

		yield* this.streamEvents(
			event => isRunTerminal(event, send.runId!),
			options.signal,
		);
	}

	async approveTool(approvalId: string): Promise<ApprovalResult> {
		return this.callApprovalRpc(RPC_TOOL_APPROVE, {approval_id: approvalId});
	}

	async rejectTool(approvalId: string, reason?: string): Promise<ApprovalResult> {
		const params: Record<string, string> = {approval_id: approvalId};
		if (reason) params.reason = reason;
		return this.callApprovalRpc(RPC_TOOL_REJECT, params);
	}

	private async callApprovalRpc(
		method: string,
		params: Record<string, string>,
	): Promise<ApprovalResult> {
		let raw: unknown;
		try {
			raw = await this.callRpc(method, params);
		} catch (error) {
			throw toApprovalError(error);
		}

		const result = raw as ApprovalRpcResult;
		const approvalId = result.approvalId ?? result.approval_id;
		if (!approvalId || !isApprovalOutcome(result.outcome)) {
			throw new ApprovalError(
				'network',
				`${method} returned unexpected result shape`,
			);
		}
		return {approvalId, outcome: result.outcome};
	}

	async close(): Promise<void> {
		this.stopOwnedBackend();
	}

	/** Restart the backend process and open a fresh session (e.g. after model env change). */
	async reconnect(): Promise<SessionInfo> {
		this.stopOwnedBackend();
		return this.connect();
	}

	private stopOwnedBackend(): void {
		const child = this.child;
		this.child = null;
		this.endpoint = '';
		this.session = null;
		this.lastEventId = '';

		if (!child) {
			return;
		}

		child.kill();
		child.removeAllListeners();
	}

	private async startBackend(): Promise<void> {
		if (this.endpoint) {
			return;
		}
		if (this.child) {
			throw new Error('backend is already starting');
		}

		const command = await resolveBackendCommand(this.backendPath);
		const child = spawn(command.file, command.args, {
			cwd: command.cwd,
			stdio: ['ignore', 'pipe', 'inherit'],
			env: process.env,
		});
		this.child = child;

		try {
			const ready = await readBootstrap(child);
			this.endpoint = ready.baseUrl.replace(/\/$/, '');
		} catch (error) {
			this.stopOwnedBackend();
			throw error;
		}
	}

	private async callRpc(method: string, params?: unknown): Promise<unknown> {
		if (!this.endpoint) {
			throw new Error('backend endpoint is not available');
		}

		const payload: JsonRpcRequest = {
			jsonrpc: '2.0',
			id: newRequestId(),
			method,
			params,
		};

		const response = await this.fetchImpl(`${this.endpoint}/rpc`, {
			method: 'POST',
			headers: {'Content-Type': 'application/json'},
			body: JSON.stringify(payload),
		});

		const body = await response.text();
		if (!response.ok) {
			throw new Error(`JSON-RPC ${method} returned HTTP ${response.status}: ${body.trim()}`);
		}

		const parsed = JSON.parse(body) as JsonRpcResponse;
		if (parsed.error) {
			throw new RpcError(parsed.error.code, parsed.error.message);
		}
		if (parsed.result === undefined) {
			throw new Error(`JSON-RPC ${method} returned no result`);
		}
		return parsed.result;
	}

	private async fetchEvents(stop?: (event: NavEvent) => boolean): Promise<NavEvent[]> {
		const events: NavEvent[] = [];
		for await (const event of this.iterSessionEvents()) {
			events.push(event);
			if (stop?.(event)) {
				return events;
			}
		}
		if (stop) {
			throw new Error('SSE stream ended before the expected event');
		}
		return events;
	}

	private async *streamEvents(
		stop: (event: NavEvent) => boolean,
		signal?: AbortSignal,
	): AsyncGenerator<NavEvent, void, void> {
		for (;;) {
			const previous = this.lastEventId;
			let sawTerminal = false;

			for await (const event of this.iterSessionEvents(signal)) {
				yield event;
				if (stop(event)) {
					sawTerminal = true;
					break;
				}
			}

			if (sawTerminal) {
				return;
			}

			if (this.lastEventId === previous) {
				throw new Error('SSE stream ended before the expected event');
			}
		}
	}

	private async *iterSessionEvents(
		signal?: AbortSignal,
	): AsyncGenerator<NavEvent, void, void> {
		if (!this.session) {
			throw new Error('session is not connected');
		}

		const headers: Record<string, string> = {};
		if (this.lastEventId) {
			headers['Last-Event-ID'] = this.lastEventId;
		}

		const response = await this.fetchImpl(
			`${this.endpoint}/sessions/${this.session.sessionId}/events`,
			{headers, signal},
		);

		if (!response.ok) {
			const body = await response.text();
			throw new Error(
				`session events returned HTTP ${response.status}: ${body.trim()}`,
			);
		}

		if (!response.body) {
			throw new Error('session events returned an empty body');
		}

		yield* readSseStream(response.body, event => {
			if (event.id) {
				this.lastEventId = event.id;
			}
		});
	}
}

export function eventText(event: NavEvent): string {
	switch (event.type) {
		case 'message.delta':
			return event.text;
		case 'part.delta':
			return event.field === 'text' ? event.delta : '';
		case 'model.text_delta':
			return event.delta;
		case 'model.reasoning_delta':
			return event.delta;
		case 'run.failed':
		case 'error':
		case 'provider.error':
			return event.message;
		default:
			return '';
	}
}

function isRunTerminal(event: NavEvent, runId: string): boolean {
	if (!('runId' in event) || event.runId !== runId) {
		return false;
	}
	return (
		event.type === 'run.completed' ||
		event.type === 'run.failed' ||
		event.type === 'run.cancelled'
	);
}

function isApprovalOutcome(value: unknown): value is ApprovalResult['outcome'] {
	return value === 'approved' || value === 'rejected';
}

function newRequestId(): string {
	const bytes = new Uint8Array(16);
	bytes.set(randomBytes(10), 6);
	const millis = BigInt(Date.now());
	bytes[0] = Number((millis >> 40n) & 0xffn);
	bytes[1] = Number((millis >> 32n) & 0xffn);
	bytes[2] = Number((millis >> 24n) & 0xffn);
	bytes[3] = Number((millis >> 16n) & 0xffn);
	bytes[4] = Number((millis >> 8n) & 0xffn);
	bytes[5] = Number(millis & 0xffn);
	bytes[6] = (bytes[6]! & 0x0f) | 0x70;
	bytes[8] = (bytes[8]! & 0x3f) | 0x80;

	const hex = [...bytes].map(byte => byte.toString(16).padStart(2, '0')).join('');
	return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function readBootstrap(child: ChildProcess): Promise<BackendReady> {
	const stdout = child.stdout;
	if (!stdout) {
		throw new Error('backend stdout is not available');
	}

	const line = await new Promise<string>((resolve, reject) => {
		const rl = createInterface({input: stdout});
		const fail = (message: string) => {
			rl.close();
			reject(new Error(message));
		};

		rl.once('line', value => {
			rl.removeAllListeners('close');
			rl.close();
			resolve(value);
		});
		rl.once('close', () => fail('backend exited without bootstrap endpoint'));
		child.once('error', error => fail(`backend failed to start: ${error.message}`));
		child.once('exit', code =>
			fail(`backend exited before bootstrap (code ${code ?? 'unknown'})`),
		);
	});

	let ready: BackendReady;
	try {
		ready = JSON.parse(line) as BackendReady;
	} catch (error) {
		throw new Error(`decode backend bootstrap: ${String(error)}`);
	}

	if (ready.type !== 'backend.ready' || !ready.baseUrl) {
		throw new Error(`unexpected backend bootstrap: ${line}`);
	}

	return ready;
}

async function resolveBackendCommand(
	backendPath: string,
): Promise<{file: string; args: string[]; cwd?: string}> {
	if (backendPath) {
		return {file: backendPath, args: ['serve-http']};
	}

	const envBackend = process.env.NAV_BACKEND?.trim();
	if (envBackend) {
		return {file: envBackend, args: ['serve-http']};
	}

	const sibling = path.join(path.dirname(process.argv[1] ?? ''), 'nav-backend');
	if (await isExecutable(sibling)) {
		return {file: sibling, args: ['serve-http']};
	}

	const manifest = await findWorkspaceManifest();
	if (manifest) {
		return {
			file: 'cargo',
			args: [
				'run',
				'--quiet',
				'--manifest-path',
				manifest,
				'-p',
				'nav-backend',
				'--',
				'serve-http',
			],
			cwd: path.dirname(manifest),
		};
	}

	return {file: 'nav-backend', args: ['serve-http']};
}

async function findWorkspaceManifest(): Promise<string | null> {
	let dir = process.cwd();
	for (;;) {
		const manifest = path.join(dir, 'Cargo.toml');
		try {
			const data = await readFile(manifest, 'utf8');
			if (data.includes('nav-backend')) {
				return manifest;
			}
		} catch {
			// keep walking
		}

		const parent = path.dirname(dir);
		if (parent === dir) {
			return null;
		}
		dir = parent;
	}
}

async function isExecutable(filePath: string): Promise<boolean> {
	try {
		await access(filePath, constants.X_OK);
		return true;
	} catch {
		return false;
	}
}

export async function* readSseStream(
	body: ReadableStream<Uint8Array>,
	onEvent: (event: NavEvent) => void,
): AsyncGenerator<NavEvent, void, void> {
	const reader = body.getReader();
	const decoder = new TextDecoder();
	const pending = {buffer: ''};

	try {
		for (;;) {
			const {done, value} = await reader.read();
			if (done) {
				break;
			}
			pending.buffer += decoder.decode(value, {stream: true});
			for (const event of takeSseBlocks(pending)) {
				onEvent(event);
				yield event;
			}
		}

		pending.buffer += decoder.decode();
		for (const event of takeSseBlocks(pending, true)) {
			onEvent(event);
			yield event;
		}
	} finally {
		reader.releaseLock();
	}
}

function* takeSseBlocks(
	pending: {buffer: string},
	flushRemainder = false,
): Generator<NavEvent, void, void> {
	const text = pending.buffer;
	let offset = 0;

	while (true) {
		const boundary = text.indexOf('\n\n', offset);
		if (boundary === -1) {
			break;
		}

		const block = text.slice(offset, boundary);
		offset = boundary + 2;
		const event = parseSseBlock(block);
		if (event) {
			yield event;
		}
	}

	pending.buffer = text.slice(offset);
	if (!flushRemainder || !pending.buffer.trim()) {
		return;
	}

	const event = parseSseBlock(pending.buffer);
	pending.buffer = '';
	if (event) {
		yield event;
	}
}

function parseSseBlock(block: string): NavEvent | null {
	let parsed: NavEvent | null = null;
	parseSse(`${block}\n\n`, event => {
		parsed = event;
		return false;
	});
	return parsed;
}

export function parseSse(
	input: string,
	emit: (event: NavEvent) => boolean,
): boolean {
	const lines = input.split(/\r?\n/);
	let current: SseEventFrame = emptyEvent();
	let dataLines: string[] = [];

	const flush = (): boolean => {
		if (!current.id && !current.type && dataLines.length === 0) {
			return false;
		}
		const event = decodeSseEvent(current, dataLines);
		current = emptyEvent();
		dataLines = [];
		return emit(event);
	};

	for (const line of lines) {
		if (line === '') {
			if (flush()) {
				return true;
			}
			continue;
		}

		if (line.startsWith('id:')) {
			current.id = line.slice(3).trim();
		} else if (line.startsWith('event:')) {
			current.type = line.slice(6).trim();
		} else if (line.startsWith('data:')) {
			dataLines.push(line.slice(5).trim());
		}
	}

	return flush();
}

function emptyEvent(): SseEventFrame {
	return {
		id: '',
		type: '',
	};
}

function decodeSseEvent(event: SseEventFrame, dataLines: string[]): NavEvent {
	let payload: EventPayload = {};
	if (dataLines.length > 0) {
		payload = JSON.parse(dataLines.join('\n')) as EventPayload;
	}

	const type = event.type || payload.type || '';
	const base = eventBase(event, payload);

	switch (type) {
		case 'session.created':
			return {...base, type};
		case 'run.started':
		case 'run.completed':
		case 'run.cancelled':
			return {...base, type, ...runFields(payload)};
		case 'model.text_delta':
		case 'model.reasoning_delta':
			return {
				...base,
				type,
				...messageFields(payload),
				delta: payload.delta || '',
			};
		case 'message.delta':
			return {
				...base,
				type,
				...messageFields(payload),
				text: payload.text || '',
			};
		case 'part.delta':
			return {
				...base,
				type,
				...partFields(payload),
				field: payload.field || '',
				delta: payload.delta || '',
			};
		case 'part.completed':
			return {
				...base,
				type,
				...partFields(payload),
			};
		case 'message.completed':
			return {
				...base,
				type,
				...messageFields(payload),
				finishReason: payload.finish_reason || '',
			};
		case 'tool.call_requested':
		case 'tool.call_started':
			return {
				...base,
				type,
				...toolFields(payload),
				name: payload.name || '',
			};
		case 'tool.call_delta':
			return {
				...base,
				type,
				...toolFields(payload),
				argumentsDelta: payload.arguments_delta || '',
			};
		case 'tool.output_delta':
			return {
				...base,
				type,
				...toolFields(payload),
				stream: payload.stream || '',
				chunk: payload.chunk || '',
			};
		case 'tool.call_completed':
			return {
				...base,
				type,
				...toolFields(payload),
				name: payload.name || '',
				arguments: payload.arguments || '',
				output: payload.output,
				outputLossy: payload.output_lossy,
			};
		case 'tool.call_failed':
			return {
				...base,
				type,
				...toolFields(payload),
				name: payload.name || '',
				errorMessage: payload.error_message || '',
				output: payload.output,
				outputLossy: payload.output_lossy,
			};
		case 'tool.approval_requested':
			return {
				...base,
				type,
				...toolFields(payload),
				approvalId: payload.approval_id || '',
				toolName: payload.tool_name || '',
				reason: payload.reason || '',
				argumentsSummary: payload.arguments_summary || '',
				riskClass: payload.risk_class,
			};
		case 'file.changed':
			return {
				...base,
				type,
				fileChangeId: payload.file_change_id || payload.change_id || '',
				path: payload.path || '',
				kind: fileChangeKind(payload.kind),
			};
		case 'run.failed':
			return {
				...base,
				type,
				...runFields(payload),
				message: payload.message || '',
			};
		case 'provider.error':
			return {
				...base,
				type,
				...runFields(payload),
				message: payload.message || '',
				status: payload.status,
				errorType: payload.error_type,
				code: payload.code,
			};
		case 'error':
			return {...base, type, message: payload.message || ''};
		default:
			return {
				...base,
				type: 'unknown',
				rawType: type,
				payload: payload as Record<string, unknown>,
			};
	}
}

function eventBase(event: SseEventFrame, payload: EventPayload): NavEventBase {
	return {
		id: event.id || payload.event_id || '',
		sessionId: payload.session_id || '',
	};
}

function runFields(payload: EventPayload): Pick<RunScopedEvent, 'runId'> {
	return {
		runId: payload.run_id || '',
	};
}

function messageFields(
	payload: EventPayload,
): Pick<MessageScopedEvent, 'runId' | 'messageId'> {
	return {
		...runFields(payload),
		messageId: payload.message_id || '',
	};
}

function partFields(
	payload: EventPayload,
): Pick<PartScopedEvent, 'turnId' | 'partId'> {
	return {
		turnId: payload.turn_id || payload.message_id || '',
		partId: payload.part_id || '',
	};
}

function toolFields(
	payload: EventPayload,
): Pick<ToolScopedEvent, 'runId' | 'toolCallId'> {
	return {
		...runFields(payload),
		toolCallId: payload.tool_call_id || '',
	};
}

function fileChangeKind(kind: string | undefined): FileChangeKind | undefined {
	if (kind === 'created' || kind === 'modified' || kind === 'deleted') {
		return kind;
	}
	return undefined;
}
