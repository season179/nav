#!/usr/bin/env bun
/**
 * UI preview — no nav-backend, no navd, no LLM.
 *
 *   bun run preview              # full layout (default)
 *   bun run preview history
 *   bun run preview composer
 *   bun run preview model
 *
 * Keys (shell scene): m model picker · e empty · c chat · q quit
 */
import React, {useState} from 'react';
import {Box, render, Text, useApp, useInput} from 'ink';
import {
	COMPOSER_HEIGHT,
	ComposerRegion,
} from './regions/composer/ComposerRegion.js';
import {HistoryRegion} from './regions/history/HistoryRegion.js';
import {ModelPickerOverlay} from './overlays/model/ModelPickerOverlay.js';
import type {ModelOption, ModelRef} from './overlays/model/load-models.js';
import type {HistoryMessage} from './regions/history/types.js';
import {theme} from './theme/index.js';
import {useTerminalSize} from './app/use-terminal-size.js';

const IDLE_HINT = 'Enter send · /model · /exit · Esc clear · Ctrl+C quit';

const SAMPLE_MODELS: ModelOption[] = [
	{provider: 'openai', model: 'gpt-4.1', label: 'openai/gpt-4.1'},
	{provider: 'anthropic', model: 'claude-sonnet-4', label: 'anthropic/claude-sonnet-4'},
	{provider: 'compatible', model: 'vendor/model', label: 'compatible/vendor/model'},
];

const CHAT_MESSAGES: HistoryMessage[] = [
	{id: '1', role: 'user', text: 'Show me a small Python snippet.'},
	{
		id: '2',
		role: 'assistant',
		text: [
			'### A tiny example',
			'',
			'Here is a function that returns the first `n` even numbers — note how the **keyword**, *string*, and `comment` tokens land on different Solarized accents.',
			'',
			'```python',
			'# return the first n even numbers',
			'def evens(n: int) -> list[int]:',
			'    return [i * 2 for i in range(n)]',
			'',
			'print(evens(5))  # [0, 2, 4, 6, 8]',
			'```',
			'',
			'- bullet one',
			'- bullet two',
		].join('\n'),
	},
	{id: '3', role: 'system', text: 'Model set to openai/gpt-4.1'},
];

const scene = process.argv[2] ?? 'shell';

function PreviewFrame({children}: {children: React.ReactNode}) {
	const {columns, rows} = useTerminalSize();
	return (
		<Box flexDirection="column" width={columns} height={rows}>
			{children}
		</Box>
	);
}

function HistoryPreview() {
	return (
		<PreviewFrame>
			<HistoryRegion messages={CHAT_MESSAGES} />
		</PreviewFrame>
	);
}

function ComposerPreview() {
	const {columns} = useTerminalSize();
	const [value, setValue] = useState('Type here — no backend attached');
	return (
		<PreviewFrame>
			<Box flexGrow={1} />
			<ComposerRegion
				value={value}
				busy={false}
				hint={IDLE_HINT}
				width={columns}
				focused
				onChange={setValue}
				onSubmit={() => {}}
			/>
		</PreviewFrame>
	);
}

function ModelPreview() {
	return (
		<PreviewFrame>
			<ModelPickerOverlay
				options={SAMPLE_MODELS}
				current={{provider: 'openai', model: 'gpt-4.1'}}
				onSelect={() => process.exit(0)}
				onCancel={() => process.exit(0)}
			/>
		</PreviewFrame>
	);
}

function ShellPreview() {
	const {exit} = useApp();
	const {columns, rows} = useTerminalSize();
	const [messages, setMessages] = useState<HistoryMessage[]>(CHAT_MESSAGES);
	const [input, setInput] = useState('');
	const [modelOpen, setModelOpen] = useState(false);
	const historyHeight = Math.max(1, rows - COMPOSER_HEIGHT);

	useInput((inputKey, key) => {
		if (modelOpen) {
			return;
		}
		if (inputKey === 'q' || (key.ctrl && inputKey === 'c')) {
			exit();
			return;
		}
		if (inputKey === 'm') {
			setModelOpen(true);
			return;
		}
		if (inputKey === 'e') {
			setMessages([]);
			return;
		}
		if (inputKey === 'c') {
			setMessages(CHAT_MESSAGES);
		}
	});

	return (
		<Box flexDirection="column" width={columns} height={rows}>
			<Box height={1} flexShrink={0} paddingX={2}>
				<Text color={theme.inactive}>
					Preview · m picker · e empty · c chat · q quit
				</Text>
			</Box>
			<Box
				flexDirection="column"
				height={historyHeight - 1}
				overflow="hidden"
				flexShrink={0}
			>
				{modelOpen ? (
					<ModelPickerOverlay
						options={SAMPLE_MODELS}
						current={{provider: 'openai', model: 'gpt-4.1'}}
						onSelect={(ref: ModelRef) => {
							setModelOpen(false);
							setMessages(previous => [
								...previous,
								{
									id: crypto.randomUUID(),
									role: 'system',
									text: `Model set to ${ref.provider}/${ref.model}`,
								},
							]);
						}}
						onCancel={() => setModelOpen(false)}
					/>
				) : (
					<HistoryRegion messages={messages} />
				)}
			</Box>
			<ComposerRegion
				value={input}
				busy={false}
				hint={modelOpen ? 'Model picker — Esc cancel' : IDLE_HINT}
				width={columns}
				focused={!modelOpen}
				onChange={setInput}
				onSubmit={text => {
					const trimmed = text.trim();
					if (!trimmed) {
						return;
					}
					setInput('');
					if (trimmed === '/exit') {
						exit();
						return;
					}
					if (trimmed === '/model') {
						setModelOpen(true);
						return;
					}
					setMessages(previous => [
						...previous,
						{id: crypto.randomUUID(), role: 'user', text: trimmed},
						{
							id: crypto.randomUUID(),
							role: 'assistant',
							text: '(preview — no backend)',
						},
					]);
				}}
			/>
		</Box>
	);
}

const views: Record<string, React.ComponentType> = {
	shell: ShellPreview,
	history: HistoryPreview,
	composer: ComposerPreview,
	model: ModelPreview,
};

const View = views[scene];
if (!View) {
	console.error(`Unknown preview scene "${scene}". Try: shell, history, composer, model`);
	process.exit(1);
}

render(<View />);
