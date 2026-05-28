declare module 'signal-exit' {
	type ExitHandler = (
		code: number | null,
		signal: NodeJS.Signals | null,
	) => void;

	type Options = {
		readonly alwaysLast?: boolean;
	};

	export default function onExit(
		handler: ExitHandler,
		options?: Options,
	): () => void;
}
