import type {FileChangeKind} from '../../backend/client.js';

export type {FileChangeKind};

type HistoryMessageBase = {
	id: string;
	contentVersion?: number;
};

export type TextHistoryMessage = HistoryMessageBase & {
	role: 'user' | 'assistant' | 'system';
	text: string;
};

export type ToolCallStatus =
	| 'requested'
	| 'running'
	| 'completed'
	| 'failed'
	| 'approval_requested';

export type ToolCallHistoryMessage = HistoryMessageBase & {
	role: 'tool_call';
	runId: string;
	toolCallId: string;
	name: string;
	arguments: string;
	status: ToolCallStatus;
	approvalId?: string;
	errorMessage?: string;
	streamingOutput?: string;
	output?: string;
	outputLossy?: boolean;
};

export type ToolResultHistoryMessage = HistoryMessageBase & {
	role: 'tool_result';
	runId: string;
	toolCallId: string;
	name: string;
	status: 'completed' | 'failed';
	text: string;
	errorMessage?: string;
};

export type FileChangedHistoryMessage = HistoryMessageBase & {
	role: 'file_changed';
	path: string;
	kind?: FileChangeKind;
};

export type HistoryMessage =
	| TextHistoryMessage
	| ToolCallHistoryMessage
	| ToolResultHistoryMessage
	| FileChangedHistoryMessage;
