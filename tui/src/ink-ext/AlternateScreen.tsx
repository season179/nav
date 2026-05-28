import React, {useEffect, useInsertionEffect, useState} from 'react';
import {Box, useStdout} from 'ink';
import onExit from 'signal-exit';

type Props = {
	readonly children: React.ReactNode;
	readonly mouseTracking?: boolean;
};

type TerminalSize = {
	readonly columns: number;
	readonly rows: number;
};

const ENTER_ALTERNATE_SCREEN = '\x1b[?1049h\x1b[2J\x1b[H';
const ENABLE_MOUSE_TRACKING = '\x1b[?1000h\x1b[?1006h';
const EXIT_ALTERNATE_SCREEN = '\x1b[?1000l\x1b[?1006l\x1b[?1049l\x1b[?25h';
const DEFAULT_COLUMNS = 80;
const DEFAULT_ROWS = 24;

export function AlternateScreen({
	children,
	mouseTracking = false,
}: Props): React.ReactElement {
	const {stdout, write} = useStdout();
	const [{columns, rows}, setSize] = useState<TerminalSize>(() =>
		getTerminalSize(stdout),
	);

	useEffect(() => {
		const updateSize = () => {
			setSize(getTerminalSize(stdout));
		};

		stdout.on('resize', updateSize);
		return () => {
			stdout.off('resize', updateSize);
		};
	}, [stdout]);

	useInsertionEffect(() => {
		let restored = false;
		const restoreTerminal = () => {
			if (restored) {
				return;
			}

			restored = true;
			stdout.write(EXIT_ALTERNATE_SCREEN);
		};
		const removeExitHandler = onExit(restoreTerminal);

		write(getEnterSequence(mouseTracking));

		return () => {
			restoreTerminal();
			removeExitHandler();
		};
	}, [mouseTracking, stdout, write]);

	return (
		<Box height={rows} width={columns} flexShrink={0}>
			{children}
		</Box>
	);
}

function getEnterSequence(mouseTracking: boolean): string {
	if (mouseTracking) {
		return ENTER_ALTERNATE_SCREEN + ENABLE_MOUSE_TRACKING;
	}

	return ENTER_ALTERNATE_SCREEN;
}

function getTerminalSize(stdout: NodeJS.WriteStream): TerminalSize {
	return {
		columns: stdout.columns ?? DEFAULT_COLUMNS,
		rows: stdout.rows ?? DEFAULT_ROWS,
	};
}
