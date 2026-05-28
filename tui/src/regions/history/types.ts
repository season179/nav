import type {FileChangeKind} from '../../backend/client.js';

export type {FileChangeKind};

export type TextHistoryMessage = {
	id: string;
	role: 'user' | 'assistant' | 'system';
	text: string;
};

export type ToolCallStatus =
	| 'requested'
	| 'running'
	| 'completed'
	| 'failed'
	| 'approval_requested';

export type ToolCallHistoryMessage = {
	id: string;
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

export type ToolResultHistoryMessage = {
	id: string;
	role: 'tool_result';
	runId: string;
	toolCallId: string;
	name: string;
	status: 'completed' | 'failed';
	text: string;
	errorMessage?: string;
};

export type FileChangedHistoryMessage = {
	id: string;
	role: 'file_changed';
	path: string;
	kind?: FileChangeKind;
};

export type HistoryMessage =
	| TextHistoryMessage
	| ToolCallHistoryMessage
	| ToolResultHistoryMessage
	| FileChangedHistoryMessage;
