import {Box, Text} from 'ink';
import TextInput from 'ink-text-input';
import type {SessionTotals} from '../../backend/client.js';
import {theme} from '../../theme/index.js';

/** Terminal rows: top rule + input + bottom rule + status. */
export const COMPOSER_HEIGHT = 4;

type Props = {
	value: string;
	busy: boolean;
	hint: string;
	width: number;
	focused: boolean;
	sessionTotals?: SessionTotals | null;
	onChange: (value: string) => void;
	onSubmit: (value: string) => void;
};

function formatCost(cost: number): string {
	if (cost < 0.01) return `$${cost.toFixed(4)}`;
	if (cost < 1) return `$${cost.toFixed(3)}`;
	return `$${cost.toFixed(2)}`;
}

function formatTokens(count: number): string {
	if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
	if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
	return String(count);
}

function HorizontalRule({width}: {width: number}) {
	return (
		<Box height={1} flexShrink={0}>
			<Text color={theme.promptBorder}>{'─'.repeat(Math.max(1, width))}</Text>
		</Box>
	);
}

/** Session usage totals, right-aligned on the status row. */
function SessionTotalsView({totals}: {totals: SessionTotals}) {
	return (
		<Box gap={2} flexShrink={0}>
			<Text color={theme.warning}>Cost: {formatCost(totals.cost)}</Text>
			<Text color={theme.info}>In: {formatTokens(totals.tokensInput)}</Text>
			<Text color={theme.success}>Out: {formatTokens(totals.tokensOutput)}</Text>
		</Box>
	);
}

export function ComposerRegion({
	value,
	busy,
	hint,
	width,
	focused,
	sessionTotals,
	onChange,
	onSubmit,
}: Props) {
	return (
		<Box
			flexDirection="column"
			height={COMPOSER_HEIGHT}
			flexShrink={0}
			width={width}
		>
			<HorizontalRule width={width} />
			<Box flexDirection="row" height={1} flexShrink={0} width={width}>
				<Text color={theme.text}>{'>'} </Text>
				<Box flexGrow={1}>
					{busy ? (
						<Text color={theme.text}>{value || ' '}</Text>
					) : (
						<TextInput
							value={value}
							onChange={onChange}
							onSubmit={onSubmit}
							focus={focused}
							showCursor
						/>
					)}
				</Box>
			</Box>
			<HorizontalRule width={width} />
			<Box
				height={1}
				flexShrink={0}
				width={width}
				justifyContent="space-between"
				gap={2}
			>
				<Box flexShrink={1} minWidth={0}>
					<Text color={theme.inactive} wrap="truncate-end">
						{busy ? 'Running…' : hint}
					</Text>
				</Box>
				{sessionTotals && <SessionTotalsView totals={sessionTotals} />}
			</Box>
		</Box>
	);
}
