export type HistoryMessage = {
	id: string;
	role: 'user' | 'assistant' | 'system';
	text: string;
};
