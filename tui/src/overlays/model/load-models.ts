import {readFile} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

export type ModelRef = {
	provider: string;
	model: string;
};

export type NavModelSettings = {
	defaultModel?: ModelRef;
	providers?: Record<
		string,
		{
			models?: Array<{id: string}>;
		}
	>;
};

export type ModelOption = {
	provider: string;
	model: string;
	label: string;
};

const NAV_MODEL_SETTINGS = 'NAV_MODEL_SETTINGS';
const NAV_MODEL_PROVIDER = 'NAV_MODEL_PROVIDER';
const NAV_MODEL = 'NAV_MODEL';

export function settingsPath(): string {
	const fromEnv = process.env[NAV_MODEL_SETTINGS]?.trim();
	if (fromEnv) {
		return expandHome(fromEnv);
	}
	return path.join(os.homedir(), '.nav', 'settings.json');
}

export function getActiveModelRef(): ModelRef | null {
	const provider = process.env[NAV_MODEL_PROVIDER]?.trim();
	const model = process.env[NAV_MODEL]?.trim();
	if (provider && model) {
		return {provider, model};
	}
	return null;
}

export function applyModelEnv(ref: ModelRef): void {
	process.env[NAV_MODEL_PROVIDER] = ref.provider;
	process.env[NAV_MODEL] = ref.model;
}

export function formatModelLabel(ref: ModelRef): string {
	return `${ref.provider}/${ref.model}`;
}

export async function loadModelSettings(): Promise<NavModelSettings> {
	const raw = await readFile(settingsPath(), 'utf8');
	return JSON.parse(raw) as NavModelSettings;
}

export async function listModelOptions(): Promise<ModelOption[]> {
	const settings = await loadModelSettings();
	const options: ModelOption[] = [];
	const seen = new Set<string>();

	for (const [providerId, provider] of Object.entries(
		settings.providers ?? {},
	)) {
		for (const entry of provider.models ?? []) {
			const key = `${providerId}\0${entry.id}`;
			if (seen.has(key)) {
				continue;
			}
			seen.add(key);
			options.push({
				provider: providerId,
				model: entry.id,
				label: formatModelLabel({provider: providerId, model: entry.id}),
			});
		}
	}

	options.sort((a, b) => a.label.localeCompare(b.label));
	return options;
}

export async function resolveCurrentModelRef(): Promise<ModelRef | null> {
	const fromEnv = getActiveModelRef();
	if (fromEnv) {
		return fromEnv;
	}

	try {
		const settings = await loadModelSettings();
		return settings.defaultModel ?? null;
	} catch {
		return null;
	}
}

function expandHome(filePath: string): string {
	if (filePath === '~') {
		return os.homedir();
	}
	if (filePath.startsWith('~/')) {
		return path.join(os.homedir(), filePath.slice(2));
	}
	return filePath;
}
