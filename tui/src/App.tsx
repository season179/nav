import React, {useEffect, useRef, useState} from 'react';
import {Box, useApp, useInput} from 'ink';
import {useTerminalSize} from './use-terminal-size.js';
import {COMPOSER_HEIGHT, Composer} from './Composer.js';
import {HistoryPane} from './HistoryPane.js';
import {
	NavBackendClient,
	eventText,
	type NavEvent,
} from './backend-client.js';
import type {HistoryMessage} from './types.js';

type Props = {
	backendPath?: string;
};

export function App({backendPath = ''}: Props) {
	const {exit} = useApp();
	const {columns, rows} = useTerminalSize();
	const clientRef = useRef(new NavBackendClient(backendPath));
	const [messages, setMessages] = useState<HistoryMessage[]>([]);
	const [input, setInput] = useState('');
	const [busy, setBusy] = useState(false);
	const [hint, setHint] = useState('Enter send · Esc clear · Ctrl+C quit');

	useEffect(() => {
		return () => {
			void clientRef.current.close();
		};
	}, []);

	useInput(
		(character, key) => {
			if (key.ctrl && character === 'c') {
				exit();
				return;
			}

			if (!busy && key.escape) {
				setInput('');
			}
		},
		{isActive: true},
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
				<HistoryPane messages={messages} />
			</Box>
			<Composer
				value={input}
				busy={busy}
				hint={hint}
				width={columns}
				onChange={setInput}
				onSubmit={submitted => {
					const text = submitted.trim();
					if (!text || busy) {
						return;
					}
					setInput('');
					void sendText(text);
				}}
			/>
		</Box>
	);

	async function sendText(text: string) {
		const assistantId = crypto.randomUUID();
		setBusy(true);
		setHint('Connecting…');
		setMessages(previous => [
			...previous,
			{id: crypto.randomUUID(), role: 'user', text},
			{id: assistantId, role: 'assistant', text: ''},
		]);

		try {
			for await (const event of clientRef.current.streamMessage(text)) {
				applyEvent(event, assistantId);
			}
		} catch (caught) {
			const message =
				caught instanceof Error ? caught.message : String(caught);
			setMessages(previous =>
				previous.map(entry =>
					entry.id === assistantId
						? {...entry, text: message}
						: entry,
				),
			);
			setHint('Error — Enter to retry');
		} finally {
			setBusy(false);
			setHint('Enter send · Esc clear · Ctrl+C quit');
		}
	}

	function applyEvent(event: NavEvent, assistantId: string) {
		if (event.type === 'model.reasoning_delta') {
			return;
		}

		if (
			event.type === 'run.failed' ||
			event.type === 'error' ||
			event.type === 'provider.error'
		) {
			const message = eventText(event) || event.message || event.type;
			setMessages(previous =>
				previous.map(entry =>
					entry.id === assistantId
						? {...entry, text: message}
						: entry,
				),
			);
			return;
		}

		const chunk = eventText(event);
		if (!chunk) {
			return;
		}

		setMessages(previous =>
			previous.map(entry =>
				entry.id === assistantId
					? {...entry, text: entry.text + chunk}
					: entry,
			),
		);
	}
}
