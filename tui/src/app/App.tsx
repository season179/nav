import React, {useEffect, useRef, useState} from 'react';
import {Box, useApp, useInput} from 'ink';
import {useTerminalSize} from './use-terminal-size.js';
import {
	COMPOSER_HEIGHT,
	ComposerRegion,
} from '../regions/composer/ComposerRegion.js';
import {VirtualHistoryRegion} from '../regions/history/VirtualHistoryRegion.js';
import {ModelPickerOverlay} from '../overlays/model/ModelPickerOverlay.js';
import {
	ConfirmationOverlay,
	type ToolApprovalRequest,
} from '../overlays/confirmation/ConfirmationOverlay.js';
import {
	NavBackendClient,
	eventText,
	type ApprovalResult,
	type NavEvent,
	type SessionInfo,
	type SessionTotals,
	type StreamMessageOptions,
} from '../backend/client.js';
import {parseSlashCommand} from '../commands/slash.js';
import {
	applyModelEnv,
	formatModelLabel,
	listModelOptions,
	resolveCurrentModelRef,
	type ModelOption,
	type ModelRef,
} from '../overlays/model/load-models.js';
import type {
	HistoryMessage,
	ToolCallHistoryMessage,
	ToolCallStatus,
} from '../regions/history/types.js';

type Props = {
	backendPath?: string;
	backendClient?: AppBackendClient;
};

export type AppBackendClient = {
	streamMessage(
		text: string,
		options: StreamMessageOptions,
	): AsyncGenerator<NavEvent, void, void>;
	approveTool(approvalId: string): Promise<ApprovalResult>;
	rejectTool(approvalId: string, reason?: string): Promise<ApprovalResult>;
	sessionTotals(): Promise<SessionTotals>;
	reconnect(): Promise<SessionInfo>;
	close(): Promise<void>;
};

const IDLE_HINT = 'Enter send · /model · /exit · Esc clear · Ctrl+C quit';

export function App({backendPath = '', backendClient}: Props) {
	const {exit} = useApp();
	const {columns, rows} = useTerminalSize();
	const clientRef = useRef<AppBackendClient>(
		backendClient ?? new NavBackendClient(backendPath),
	);
	const approvalInFlightRef = useRef(false);
	const activeTurnAbortRef = useRef<AbortController | null>(null);
	const [messages, setMessages] = useState<HistoryMessage[]>([]);
	const [input, setInput] = useState('');
	const [busy, setBusy] = useState(false);
	const [hint, setHint] = useState(IDLE_HINT);
	const [modelPickerOpen, setModelPickerOpen] = useState(false);
	const [approvalRequest, setApprovalRequest] =
		useState<ToolApprovalRequest | null>(null);
	const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
	const [currentModel, setCurrentModel] = useState<ModelRef | null>(null);
	const [sessionTotals, setSessionTotals] = useState<SessionTotals | null>(null);

	useEffect(() => {
		void resolveCurrentModelRef().then(setCurrentModel);
		return () => {
			void clientRef.current.close();
		};
	}, []);

	useInput(
		(character, key) => {
			if (key.ctrl && character === 'c') {
				activeTurnAbortRef.current?.abort();
				void clientRef.current.close();
				exit();
				return;
			}
			if (modelPickerOpen) {
				return;
			}
			if (!busy && key.escape) {
				setInput('');
			}
		},
	);

	const historyHeight = Math.max(1, rows - COMPOSER_HEIGHT);

	return (
		<Box flexDirection="column" width={columns} height={rows}>
			<Box
				flexDirection="column"
				height={historyHeight}
				overflow="hidden"
				flexShrink={0}
			>
				{renderMainRegion()}
			</Box>
			<ComposerRegion
				value={input}
				busy={busy}
				hint={hint}
				width={columns}
				focused={!modelPickerOpen && !approvalRequest}
				sessionTotals={sessionTotals}
				onChange={setInput}
				onSubmit={submitted => {
					void handleSubmit(submitted);
				}}
			/>
		</Box>
	);

	async function handleSubmit(submitted: string) {
		const text = submitted.trim();
		if (!text || busy) {
			return;
		}

		const slash = parseSlashCommand(text);
		if (slash) {
			setInput('');
			await runSlashCommand(slash);
			return;
		}

		setInput('');
		await sendText(text);
	}

	function renderMainRegion(): React.JSX.Element {
		if (approvalRequest) {
			return (
				<ConfirmationOverlay
					request={approvalRequest}
					onApprove={() => {
						void answerApproval('approve');
					}}
					onReject={() => {
						void answerApproval('reject');
					}}
				/>
			);
		}

		if (modelPickerOpen) {
			return (
				<ModelPickerOverlay
					options={modelOptions}
					current={currentModel}
					onSelect={ref => {
						void applyModelSelection(ref);
					}}
					onCancel={() => {
						setModelPickerOpen(false);
						setHint(IDLE_HINT);
					}}
				/>
			);
		}

		return <VirtualHistoryRegion messages={messages} height={historyHeight} />;
	}

	async function runSlashCommand(
		slash: NonNullable<ReturnType<typeof parseSlashCommand>>,
	) {
		switch (slash.kind) {
			case 'exit':
				exit();
				return;
			case 'model':
				await openModelPicker();
				return;
			case 'unknown':
				pushSystem(`Unknown command: /${slash.name}`);
				return;
		}
	}

	async function openModelPicker() {
		try {
			const [options, current] = await Promise.all([
				listModelOptions(),
				resolveCurrentModelRef(),
			]);
			setModelOptions(options);
			setCurrentModel(current);
			setModelPickerOpen(true);
			setHint('Model picker — Esc cancel');
		} catch (caught) {
			const message =
				caught instanceof Error ? caught.message : String(caught);
			pushSystem(`Could not load models: ${message}`);
		}
	}

	async function applyModelSelection(ref: ModelRef) {
		setModelPickerOpen(false);
		setBusy(true);
		setHint('Switching model…');

		try {
			applyModelEnv(ref);
			await clientRef.current.reconnect();
			setCurrentModel(ref);
			pushSystem(`Model set to ${formatModelLabel(ref)}`);
		} catch (caught) {
			const message =
				caught instanceof Error ? caught.message : String(caught);
			pushSystem(`Failed to switch model: ${message}`);
		} finally {
			setBusy(false);
			setHint(IDLE_HINT);
		}
	}

	async function answerApproval(decision: 'approve' | 'reject') {
		const request = approvalRequest;
		if (!request || approvalInFlightRef.current) {
			return;
		}

		approvalInFlightRef.current = true;
		setHint('Sending confirmation…');
		try {
			if (decision === 'approve') {
				await clientRef.current.approveTool(request.approvalId);
			} else {
				await clientRef.current.rejectTool(request.approvalId);
			}
			setApprovalRequest(null);
			setHint('Running…');
		} catch (caught) {
			const message =
				caught instanceof Error ? caught.message : String(caught);
			pushSystem(`Confirmation failed: ${message}`);
			setHint('Confirmation failed — try again');
		} finally {
			approvalInFlightRef.current = false;
		}
	}

	function pushSystem(text: string) {
		setMessages(previous => [
			...previous,
			{id: crypto.randomUUID(), contentVersion: 1, role: 'system', text},
		]);
	}

	async function sendText(text: string) {
		const assistantId = crypto.randomUUID();
		const controller = new AbortController();
		activeTurnAbortRef.current = controller;
		setBusy(true);
		setHint('Connecting…');
		setMessages(previous => [
			...previous,
			{id: crypto.randomUUID(), contentVersion: 1, role: 'user', text},
			{id: assistantId, contentVersion: 1, role: 'assistant', text: ''},
		]);

		try {
			for await (const event of clientRef.current.streamMessage(text, {
				signal: controller.signal,
			})) {
				applyEvent(event, assistantId);
			}
		} catch (caught) {
			if (controller.signal.aborted) {
				return;
			}
			const message =
				caught instanceof Error ? caught.message : String(caught);
			setMessages(previous =>
				previous.map(entry =>
					entry.id === assistantId && entry.role === 'assistant'
						? {
								...entry,
								contentVersion: nextContentVersion(entry),
								text: message,
							}
						: entry,
				),
			);
			setHint('Error — Enter to retry');
		} finally {
			if (activeTurnAbortRef.current === controller) {
				activeTurnAbortRef.current = null;
			}
			if (!controller.signal.aborted) {
				setBusy(false);
				setHint(IDLE_HINT);
			}
		}
	}

	function applyEvent(event: NavEvent, assistantId: string) {
		if (event.type === 'tool.approval_requested') {
			setApprovalRequest(approvalRequestFromEvent(event));
			setHint('Confirm tool request');
		} else if (event.type === 'session.totals_updated') {
			setSessionTotals({
				cost: event.cost,
				tokensInput: event.tokensInput,
				tokensOutput: event.tokensOutput,
				tokensReasoning: event.tokensReasoning,
				tokensCacheRead: event.tokensCacheRead,
				tokensCacheWrite: event.tokensCacheWrite,
			});
		} else if (isRunTerminalEvent(event)) {
			setApprovalRequest(null);
		}
		setMessages(previous =>
			applyEventToHistory(previous, event, assistantId),
		);
	}
}

export function applyEventToHistory(
	messages: HistoryMessage[],
	event: NavEvent,
	assistantId: string,
	warn: (message: string) => void = console.warn,
): HistoryMessage[] {
	switch (event.type) {
		case 'session.created':
		case 'session.totals_updated':
		case 'run.started':
		case 'message.completed':
		case 'run.completed':
		case 'run.cancelled':
		case 'model.reasoning_delta':
			return messages;
		case 'message.delta':
		case 'model.text_delta':
			return appendAssistantText(messages, assistantId, eventText(event));
		case 'part.delta':
			return applyPartDelta(messages, event, assistantId);
		case 'part.completed':
			return completePart(messages, event);
		case 'run.failed':
		case 'error':
		case 'provider.error':
			return replaceAssistantText(
				messages,
				assistantId,
				eventText(event) || event.message || event.type,
			);
		case 'tool.call_requested':
			return upsertToolCall(messages, event, {
				status: 'requested',
				name: event.name,
			});
		case 'tool.call_started':
			return upsertToolCall(messages, event, {
				status: 'running',
				name: event.name,
			});
		case 'tool.call_delta':
			return upsertToolCall(messages, event, {
				status: 'running',
				argumentsDelta: event.argumentsDelta,
			});
		case 'tool.output_delta':
			return upsertToolCall(messages, event, {
				status: 'running',
				outputDelta: event.chunk,
			});
		case 'tool.call_completed':
			return upsertToolCall(messages, event, {
				status: 'completed',
				name: event.name,
				arguments: event.arguments,
				output: event.output,
				outputLossy: event.outputLossy,
			});
		case 'tool.call_failed': {
			const withCall = upsertToolCall(messages, event, {
				status: 'failed',
				name: event.name,
				errorMessage: event.errorMessage,
				output: event.output,
				outputLossy: event.outputLossy,
			});
			return [
				...withCall,
				{
					id: event.id || `${event.toolCallId}-failed`,
					contentVersion: 1,
					role: 'tool_result',
					runId: event.runId,
					toolCallId: event.toolCallId,
					name: event.name,
					status: 'failed',
					text: event.errorMessage,
					errorMessage: event.errorMessage,
				},
			];
		}
		case 'tool.approval_requested':
			return upsertToolCall(messages, event, {
				status: 'approval_requested',
				name: event.toolName,
				approvalId: event.approvalId,
			});
		case 'file.changed':
			return [
				...messages,
				{
					id: event.fileChangeId || event.id,
					contentVersion: 1,
					role: 'file_changed',
					path: event.path || '',
					kind: event.kind,
				},
			];
		case 'unknown':
			warn(`Unknown nav event type: ${event.rawType || '(missing)'}`);
			return messages;
		default: {
			const exhaustive: never = event;
			warn(`Unhandled nav event: ${JSON.stringify(exhaustive)}`);
			return messages;
		}
	}
}

function isRunTerminalEvent(
	event: NavEvent,
): event is Extract<
	NavEvent,
	{type: 'run.completed' | 'run.failed' | 'run.cancelled'}
> {
	return (
		event.type === 'run.completed' ||
		event.type === 'run.failed' ||
		event.type === 'run.cancelled'
	);
}

function approvalRequestFromEvent(
	event: Extract<NavEvent, {type: 'tool.approval_requested'}>,
): ToolApprovalRequest {
	return {
		approvalId: event.approvalId,
		toolCallId: event.toolCallId,
		toolName: event.toolName,
		reason: event.reason,
		argumentsSummary: event.argumentsSummary,
		riskClass: event.riskClass,
	};
}

function appendAssistantText(
	messages: HistoryMessage[],
	assistantId: string,
	chunk: string,
): HistoryMessage[] {
	if (!chunk) {
		return messages;
	}

	return updateAssistantText(messages, assistantId, text => text + chunk);
}

function applyPartDelta(
	messages: HistoryMessage[],
	event: Extract<NavEvent, {type: 'part.delta'}>,
	assistantId: string,
): HistoryMessage[] {
	switch (event.field) {
		case 'text':
			return appendTextPartDelta(
				messages,
				assistantId,
				event.partId,
				event.delta,
			);
		case 'arguments':
			return upsertToolCall(
				messages,
				{...event, runId: '', toolCallId: event.partId},
				{
					status: 'running',
					partId: event.partId,
					argumentsDelta: event.delta,
				},
			);
		default:
			return messages;
	}
}

function completePart(
	messages: HistoryMessage[],
	event: Extract<NavEvent, {type: 'part.completed'}>,
): HistoryMessage[] {
	const index = messages.findIndex(entry =>
		isMatchingToolCall(entry, event.partId, event.partId),
	);
	if (index === -1) {
		return messages;
	}

	return messages.map((entry, entryIndex) => {
		if (entryIndex !== index || entry.role !== 'tool_call') {
			return entry;
		}
		return {
			...entry,
			contentVersion: nextContentVersion(entry),
			status: 'completed',
		};
	});
}

function appendTextPartDelta(
	messages: HistoryMessage[],
	assistantId: string,
	partId: string,
	delta: string,
): HistoryMessage[] {
	if (!partId || !delta) {
		return messages;
	}

	const existingIndex = messages.findIndex(entry =>
		isAssistantPart(entry, partId),
	);
	if (existingIndex !== -1) {
		return messages.map((entry, entryIndex) => {
			if (entryIndex !== existingIndex || entry.role !== 'assistant') {
				return entry;
			}
			return {
				...entry,
				contentVersion: nextContentVersion(entry),
				partId,
				text: entry.text + delta,
			};
		});
	}

	const placeholderIndex = messages.findIndex(
		entry =>
			entry.id === assistantId &&
			entry.role === 'assistant' &&
			entry.text === '',
	);
	if (placeholderIndex !== -1) {
		const placeholder = messages[placeholderIndex];
		if (placeholder.role !== 'assistant') {
			return messages;
		}

		const updatedPlaceholder = {
			...placeholder,
			partId,
			contentVersion: nextContentVersion(placeholder),
			text: delta,
		};
		return replaceFloatingToEnd(messages, placeholderIndex, updatedPlaceholder);
	}

	return [
		...messages,
		{
			id: partId,
			partId,
			contentVersion: 1,
			role: 'assistant',
			text: delta,
		},
	];
}

function replaceAssistantText(
	messages: HistoryMessage[],
	assistantId: string,
	text: string,
): HistoryMessage[] {
	return updateAssistantText(messages, assistantId, () => text);
}

function updateAssistantText(
	messages: HistoryMessage[],
	assistantId: string,
	updateText: (text: string) => string,
): HistoryMessage[] {
	const index = messages.findIndex(
		entry => entry.id === assistantId && entry.role === 'assistant',
	);
	if (index === -1) {
		return messages;
	}

	const assistant = messages[index];
	if (assistant.role !== 'assistant') {
		return messages;
	}

	const updatedAssistant = {
		...assistant,
		contentVersion: nextContentVersion(assistant),
		text: updateText(assistant.text),
	};
	return replaceFloatingToEnd(messages, index, updatedAssistant);
}

// Replace the cell at `index` with `updated`. The live backend emits each turn's
// text before that turn's tool calls, and every turn of a run accumulates into
// one assistant cell. Whenever fresh text arrives while tool calls (or other
// cells) already sit after it, float the cell to the end so the latest assistant
// prose — including the final answer — renders chronologically below the tools
// it followed, rather than staying anchored above them. A cell that is already
// last keeps its place.
function replaceFloatingToEnd(
	messages: HistoryMessage[],
	index: number,
	updated: HistoryMessage,
): HistoryMessage[] {
	if (index < messages.length - 1) {
		return [
			...messages.slice(0, index),
			...messages.slice(index + 1),
			updated,
		];
	}

	return messages.map((entry, entryIndex) =>
		entryIndex === index ? updated : entry,
	);
}

type ToolCallUpdate = {
	status: ToolCallStatus;
	partId?: string;
	name?: string;
	arguments?: string;
	argumentsDelta?: string;
	approvalId?: string;
	errorMessage?: string;
	outputDelta?: string;
	output?: string;
	outputLossy?: boolean;
};

type ToolEvent = {
	id: string;
	runId: string;
	toolCallId: string;
};

function upsertToolCall(
	messages: HistoryMessage[],
	event: ToolEvent,
	update: ToolCallUpdate,
): HistoryMessage[] {
	const toolCallId = event.toolCallId || event.id;
	const index = messages.findIndex(entry =>
		isMatchingToolCall(entry, toolCallId, update.partId),
	);

	const buildMessage = (): ToolCallHistoryMessage => ({
		id: toolCallId,
		partId: update.partId,
		contentVersion: 1,
		role: 'tool_call',
		runId: event.runId,
		toolCallId,
		name: update.name ?? '',
		arguments: initialToolArguments(update),
		status: update.status,
		approvalId: update.approvalId,
		errorMessage: update.errorMessage,
		streamingOutput: update.outputDelta,
		output: update.output,
		outputLossy: update.outputLossy,
	});

	if (index === -1) {
		return [...messages, buildMessage()];
	}

	return messages.map((entry, entryIndex) => {
		if (entryIndex !== index || entry.role !== 'tool_call') {
			return entry;
		}

		return {
			...entry,
			contentVersion: nextContentVersion(entry),
			partId: update.partId ?? entry.partId,
			runId: event.runId || entry.runId,
			name: update.name || entry.name,
			arguments: updateToolArguments(entry.arguments, update),
			status: update.status,
			approvalId: update.approvalId ?? entry.approvalId,
			errorMessage: update.errorMessage ?? entry.errorMessage,
			streamingOutput: updateStreamingOutput(entry.streamingOutput, update),
			output: update.output ?? entry.output,
			outputLossy: update.outputLossy ?? entry.outputLossy,
		};
	});
}

function initialToolArguments(update: ToolCallUpdate): string {
	return update.arguments ?? update.argumentsDelta ?? '';
}

function updateToolArguments(current: string, update: ToolCallUpdate): string {
	if (update.arguments !== undefined) {
		return update.arguments;
	}
	if (update.argumentsDelta !== undefined) {
		return current + update.argumentsDelta;
	}
	return current;
}

function updateStreamingOutput(
	current: string | undefined,
	update: ToolCallUpdate,
): string | undefined {
	if (update.output !== undefined) {
		return undefined;
	}
	if (update.outputDelta !== undefined) {
		return `${current ?? ''}${update.outputDelta}`;
	}
	return current;
}

function isAssistantPart(entry: HistoryMessage, partId: string): boolean {
	return (
		entry.role === 'assistant' &&
		(entry.partId === partId || entry.id === partId)
	);
}

function isMatchingToolCall(
	entry: HistoryMessage,
	toolCallId: string,
	partId?: string,
): boolean {
	if (entry.role !== 'tool_call') {
		return false;
	}

	return (
		entry.toolCallId === toolCallId ||
		(partId !== undefined && (entry.partId === partId || entry.id === partId))
	);
}

function nextContentVersion(message: HistoryMessage): number {
	return (message.contentVersion ?? 0) + 1;
}
