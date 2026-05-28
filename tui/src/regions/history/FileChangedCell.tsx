import React from 'react';
import {Box, Text} from 'ink';
import {theme} from '../../theme/index.js';
import type {FileChangedHistoryMessage, FileChangeKind} from './types.js';

const KIND_STYLES: Record<FileChangeKind, {glyph: string; color: string}> = {
	created: {glyph: '+', color: theme.success},
	modified: {glyph: '~', color: theme.accent},
	deleted: {glyph: '-', color: theme.error},
};

const UNKNOWN_STYLE = {glyph: '·', color: theme.inactive};

/**
 * v1 placeholder for `file.changed`: a quiet single-line chip showing the
 * kind glyph + label + path. Future overlays (diff view, tree refresh) can
 * replace this without disturbing the surrounding history layout.
 */
export function FileChangedCell({
	message,
}: {
	message: FileChangedHistoryMessage;
}): React.JSX.Element {
	const {kind, path} = message;
	const {glyph, color} = kind ? KIND_STYLES[kind] : UNKNOWN_STYLE;
	const label = kind ? ` ${kind}  ` : '  ';
	const displayPath = path || '(unknown)';

	return (
		<Box marginBottom={1}>
			<Text color={color}>{glyph}</Text>
			<Text color={theme.inactive}>{label}{displayPath}</Text>
		</Box>
	);
}
