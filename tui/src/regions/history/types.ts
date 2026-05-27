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

export type HistoryMessage =
	| TextHistoryMessage
	| ToolCallHistoryMessage
	| ToolResultHistoryMessage;
