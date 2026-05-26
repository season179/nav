import {useMemo} from 'react';
import {Box, Text} from 'ink';
import {marked, type MarkedToken, type Token, type Tokens} from 'marked';
import {highlight, supportsLanguage} from 'cli-highlight';
import {solarizedSyntaxTheme} from './syntax-theme.js';
import {theme} from '../theme/index.js';

const SOLARIZED_CYAN = '#2aa198'; // inline code
const SOLARIZED_BLUE = '#268bd2'; // links

// Strip C0 controls (other than \t, \n, \r), DEL, and the C1 range. Without
// this, an assistant message that contains a literal ESC byte (or 8-bit CSI
// `0x9B` on xterm-with-8bit-controls) would reach the terminal as a live
// escape sequence: clearing the screen, moving the cursor, or — with
// `CSI 6n` — provoking a position report on stdin.
const CONTROL_CHARS = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F-\x9F]/g;

function sanitize(source: string): string {
	return source.replace(CONTROL_CHARS, '');
}

type Props = {
	source: string;
};

export function Markdown({source}: Props) {
	const tokens = useMemo(() => marked.lexer(sanitize(source)), [source]);
	// Whitespace-only input lexes to an empty token list. Returning an empty
	// Box still owns vertical layout (and combines with the caller's
	// marginBottom) — a single-character placeholder keeps the row inert.
	if (tokens.length === 0) {
		return <Text color={theme.text}> </Text>;
	}
	return (
		<Box flexDirection="column">
			{tokens.map((token, i) => (
				<BlockToken key={i} token={token} />
			))}
		</Box>
	);
}

// nav doesn't register marked extensions, so every block from the lexer is a
// MarkedToken (the discriminated union) rather than a Tokens.Generic. Cast
// once here so the switch arms get clean narrowing without per-case casts.
function BlockToken({token: raw}: {token: Token}) {
	const token = raw as MarkedToken;
	switch (token.type) {
		case 'heading':
			return (
				<Text bold color={theme.accent}>
					<Inline tokens={token.tokens ?? []} />
				</Text>
			);
		case 'paragraph':
			return (
				<Text wrap="wrap" color={theme.text}>
					<Inline tokens={token.tokens ?? []} />
				</Text>
			);
		case 'code':
			return <CodeBlock token={token} />;
		case 'blockquote':
			return <Blockquote token={token} />;
		case 'list':
			return <List token={token} />;
		case 'table':
			return <Table token={token} />;
		case 'def':
			// Link reference definitions (`[label]: url`) are metadata; the
			// reference resolution emits `link` tokens elsewhere. Render
			// nothing so the source line doesn't leak into chat.
			return null;
		case 'hr':
			// Use a top-border-only Box so the rule fills the parent width
			// instead of overflowing narrow panes with a fixed-width string.
			return (
				<Box
					borderStyle="single"
					borderTop
					borderBottom={false}
					borderLeft={false}
					borderRight={false}
					borderColor={theme.inactive}
				/>
			);
		case 'space':
			return <Box height={1} />;
		case 'checkbox':
			// Stray block-level checkbox (defensive — ListItem renders the
			// marker, this just keeps the default arm from leaking `[x] `).
			return null;
		case 'html':
			return <Text color={theme.inactive}>{token.text}</Text>;
		case 'text':
			return (
				<Text wrap="wrap" color={theme.text}>
					{token.text}
				</Text>
			);
		default:
			return (
				<Text wrap="wrap" color={theme.text}>
					{token.raw}
				</Text>
			);
	}
}

function CodeBlock({token}: {token: Tokens.Code}) {
	const lang = (token.lang ?? '').trim().split(/\s+/)[0] ?? '';
	const language = lang && supportsLanguage(lang) ? lang : 'plaintext';
	const painted = highlight(token.text, {
		language,
		theme: solarizedSyntaxTheme,
		ignoreIllegals: true,
	});
	return (
		<Box flexDirection="column" paddingX={1} backgroundColor={theme.subtle}>
			<Text>{painted}</Text>
		</Box>
	);
}

function Blockquote({token}: {token: Tokens.Blockquote}) {
	// A left-border-only Box draws the gutter for the full height of the
	// quoted content, so wrapped paragraphs and nested blocks all sit beside
	// a continuous bar — not just the first physical row.
	return (
		<Box
			flexDirection="row"
			borderStyle="single"
			borderTop={false}
			borderBottom={false}
			borderLeft
			borderRight={false}
			borderColor={theme.inactive}
			paddingLeft={1}
		>
			<Box flexDirection="column" flexGrow={1}>
				{(token.tokens ?? []).map((t, i) => (
					<BlockToken key={i} token={t} />
				))}
			</Box>
		</Box>
	);
}

function Table({token}: {token: Tokens.Table}) {
	// Align columns: every cell in column N gets the same width, computed as
	// the longest plain-text content in that column. Without this, each cell
	// sizes to its own content and the `│` separators don't line up across
	// rows. `cell.text` is the raw source so styled cells (e.g. `**bold**`)
	// over-allocate by a few chars — acceptable for a TUI fallback.
	const columnCount = token.header.length;
	const columnWidths = Array<number>(columnCount).fill(0);
	const measure = (cells: Tokens.TableCell[]) => {
		cells.forEach((cell, ci) => {
			if (ci < columnCount) {
				columnWidths[ci] = Math.max(columnWidths[ci]!, cell.text.length);
			}
		});
	};
	measure(token.header);
	token.rows.forEach(measure);

	const SEPARATOR_WIDTH = 3; // " │ "
	const renderRow = (cells: Tokens.TableCell[], rowKey: string, bold: boolean) => (
		<Box key={rowKey} flexDirection="row">
			{cells.map((cell, ci) => {
				const contentWidth = columnWidths[ci] ?? cell.text.length;
				const boxWidth = contentWidth + (ci > 0 ? SEPARATOR_WIDTH : 0);
				return (
					<Box key={ci} flexDirection="row" width={boxWidth} flexShrink={1}>
						{ci > 0 && <Text color={theme.inactive}> │ </Text>}
						<Text color={theme.text} bold={bold} wrap="wrap">
							<Inline tokens={cell.tokens} />
						</Text>
					</Box>
				);
			})}
		</Box>
	);
	return (
		<Box flexDirection="column">
			{renderRow(token.header, 'h', true)}
			{token.rows.map((row, ri) => renderRow(row, `r${ri}`, false))}
		</Box>
	);
}

function List({token}: {token: Tokens.List}) {
	// marked types `start` as `number | ""` — `""` means "no explicit start".
	// Use a type guard, not a falsy-or, so a list literally starting at `0.`
	// renders as `0., 1., 2.` instead of `1., 2., 3.`.
	const start = typeof token.start === 'number' ? token.start : 1;
	return (
		<Box flexDirection="column">
			{token.items.map((item, i) => {
				const marker = token.ordered ? `${start + i}.` : '•';
				return <ListItem key={i} item={item} marker={marker} />;
			})}
		</Box>
	);
}

function ListItem({
	item,
	marker,
}: {
	item: Tokens.ListItem;
	marker: string;
}) {
	// GFM task items: replace the bullet with a checkbox glyph and drop the
	// leading `checkbox` token from the children so its `[x] ` raw form
	// doesn't bleed through the default switch arm.
	const taskMarker = item.task ? (item.checked ? '☒' : '☐') : marker;
	const children = (item.tokens ?? []).filter(t => t.type !== 'checkbox');
	return (
		<Box flexDirection="row">
			<Text color={theme.accent}>{taskMarker} </Text>
			<Box flexDirection="column" flexGrow={1}>
				{children.map((t, i) => {
					// marked wraps a list item's prose in a block-level "text"
					// token whose `tokens` array holds the real inline tokens.
					// Render those inline instead of recursing into a paragraph.
					if (t.type === 'text' && 'tokens' in t && t.tokens) {
						return (
							<Text key={i} wrap="wrap" color={theme.text}>
								<Inline tokens={t.tokens} />
							</Text>
						);
					}
					return <BlockToken key={i} token={t} />;
				})}
			</Box>
		</Box>
	);
}

function Inline({tokens}: {tokens: Token[]}) {
	return (
		<>
			{tokens.map((token, i) => (
				<InlineToken key={i} token={token} />
			))}
		</>
	);
}

function InlineToken({token: raw}: {token: Token}) {
	const token = raw as MarkedToken;
	switch (token.type) {
		case 'text':
			return <>{token.text}</>;
		case 'escape':
			return <>{token.text}</>;
		case 'strong':
			return (
				<Text bold>
					<Inline tokens={token.tokens ?? []} />
				</Text>
			);
		case 'em':
			return (
				<Text italic>
					<Inline tokens={token.tokens ?? []} />
				</Text>
			);
		case 'del':
			return (
				<Text strikethrough>
					<Inline tokens={token.tokens ?? []} />
				</Text>
			);
		case 'checkbox':
			// In loose task lists, marked nests the checkbox inside a
			// paragraph's inline tokens. ListItem already renders the marker
			// from `item.task`/`item.checked`, so this arm just elides the
			// duplicate token instead of letting the default leak `[x] ` raw.
			return null;
		case 'codespan':
			return <Text color={SOLARIZED_CYAN}>{token.text}</Text>;
		case 'link':
			return (
				<Text color={SOLARIZED_BLUE} underline>
					<Inline tokens={token.tokens ?? []} />
				</Text>
			);
		case 'image': {
			// Terminals can't show inline images; emit a labeled placeholder
			// instead of leaking raw `![alt](url)`. Prefer alt text (which may
			// contain inline markdown), fall back to the URL, then to a static
			// "image" label so the reader at least sees something was elided.
			const alt = token.tokens ?? [];
			const hasAlt = alt.length > 0;
			return (
				<Text color={SOLARIZED_BLUE}>
					[image: {hasAlt ? <Inline tokens={alt} /> : token.href || 'image'}]
				</Text>
			);
		}
		case 'br':
			return <>{'\n'}</>;
		case 'html':
			return <>{token.text}</>;
		default:
			return <>{token.raw}</>;
	}
}
