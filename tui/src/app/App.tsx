import React, {useEffect, useRef, useState} from 'react';
import {Box, useApp, useInput} from 'ink';
import {useTerminalSize} from './use-terminal-size.js';
import {
	COMPOSER_HEIGHT,
	ComposerRegion,
} from '../regions/composer/ComposerRegion.js';
import {HistoryRegion} from '../regions/history/HistoryRegion.js';
import {ModelPickerOverlay} from '../overlays/model/ModelPickerOverlay.js';
import {
	NavBackendClient,
	eventText,
	type NavEvent,
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
import type {HistoryMessage} from '../regions/history/types.js';

type Props = {
	backendPath?: string;
};

const IDLE_HINT = 'Enter send · /model · /exit · Esc clear · Ctrl+C quit';

export function App({backendPath = ''}: Props) {
	const {exit} = useApp();
	const {columns, rows} = useTerminalSize();
	const clientRef = useRef(new NavBackendClient(backendPath));
	const [messages, setMessages] = useState<HistoryMessage[]>([]);
	const [input, setInput] = useState('');
	const [busy, setBusy] = useState(false);
	const [hint, setHint] = useState(IDLE_HINT);
	const [modelPickerOpen, setModelPickerOpen] = useState(false);
	const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
	const [currentModel, setCurrentModel] = useState<ModelRef | null>(null);

	useEffect(() => {
		void resolveCurrentModelRef().then(setCurrentModel);
		return () => {
			void clientRef.current.close();
		};
	}, []);

	useInput(
		(character, key) => {
			if (modelPickerOpen) {
				return;
			}
			if (key.ctrl && character === 'c') {
				exit();
				return;
			}
			if (!busy && key.escape) {
				setInput('');
			}
		},
		{isActive: !modelPickerOpen},
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
				{modelPickerOpen ? (
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
				) : (
					<HistoryRegion messages={messages} height={historyHeight} />
				)}
			</Box>
			<ComposerRegion
				value={input}
				busy={busy}
				hint={hint}
				width={columns}
				focused={!modelPickerOpen}
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

	function pushSystem(text: string) {
		setMessages(previous => [
			...previous,
			{id: crypto.randomUUID(), role: 'system', text},
		]);
	}

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
			setHint(IDLE_HINT);
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
