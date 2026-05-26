import {describe, expect, test} from 'bun:test';
import {render} from 'ink-testing-library';
import {highlight} from 'cli-highlight';
import {Markdown} from './Markdown.js';
import {solarizedSyntaxTheme} from './syntax-theme.js';

const ANSI_ESCAPE = /\x1b\[[0-9;]+m/;

describe('Markdown renderer — structural', () => {
	test('strips heading markers and renders the title text', () => {
		const {lastFrame} = render(<Markdown source={'### Hello'} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('Hello');
		expect(frame).not.toContain('###');
	});

	test('strips bold markers', () => {
		const {lastFrame} = render(<Markdown source={'**loud**'} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('loud');
		expect(frame).not.toContain('**');
	});

	test('strips inline-code backticks', () => {
		const {lastFrame} = render(<Markdown source={'use `foo()` here'} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('foo()');
		expect(frame).not.toContain('`');
	});

	test('consumes triple-backtick fences and shows the code body', () => {
		const source = ['```python', 'def f():', '    return 1', '```'].join(
			'\n',
		);
		const {lastFrame} = render(<Markdown source={source} />);
		const frame = lastFrame() ?? '';
		expect(frame).not.toContain('```');
		expect(frame).toContain('def');
		expect(frame).toContain('return');
	});

	test('renders bullet lists with • marker', () => {
		const {lastFrame} = render(<Markdown source={'- alpha\n- beta'} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('• alpha');
		expect(frame).toContain('• beta');
	});

	test('handles unknown code-fence languages without throwing', () => {
		const source = ['```nosuchlang', 'just text', '```'].join('\n');
		const {lastFrame} = render(<Markdown source={source} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('just text');
		expect(frame).not.toContain('```');
	});

	test('ordered list starting at 0 keeps the zero-based numbering', () => {
		// Falsy-zero regression: `Number(start || 1)` would renumber 0/1/2 → 1/2/3.
		const {lastFrame} = render(
			<Markdown source={'0. zero\n1. one\n2. two'} />,
		);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('0. zero');
		expect(frame).toContain('1. one');
		expect(frame).toContain('2. two');
	});

	test('renders GFM tables instead of leaking pipe-and-dash source', () => {
		const source = '| a | b |\n|---|---|\n| 1 | 2 |';
		const {lastFrame} = render(<Markdown source={source} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('a');
		expect(frame).toContain('b');
		expect(frame).toContain('1');
		expect(frame).toContain('2');
		// The header-separator row must not appear in the output.
		expect(frame).not.toContain('---');
	});

	test('renders inline images as a labeled placeholder', () => {
		const {lastFrame} = render(
			<Markdown source={'see ![a cat](https://x/cat.png) here'} />,
		);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('[image: a cat]');
		expect(frame).not.toContain('https://x/cat.png');
		expect(frame).not.toContain('![');
	});

	test('strips C0 control characters from source before rendering', () => {
		// A bare ESC followed by a CSI sequence would otherwise reach the terminal.
		const dangerous = 'before\x1b[2Jafter';
		const {lastFrame} = render(<Markdown source={dangerous} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('before');
		expect(frame).toContain('after');
		expect(frame).not.toContain('\x1b');
	});

	test('link-reference definitions do not leak the source line', () => {
		// `[label]: url` is metadata, not user-visible content.
		const {lastFrame} = render(
			<Markdown source={'See [docs][1].\n\n[1]: https://example.com/'} />,
		);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('docs');
		expect(frame).not.toContain('[1]:');
		expect(frame).not.toContain('https://example.com');
	});

	test('strips 8-bit C1 control characters too', () => {
		// 0x9B is the 8-bit form of CSI on terminals with 8-bit controls on.
		const dangerous = `before6nafter`;
		const {lastFrame} = render(<Markdown source={dangerous} />);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('before');
		expect(frame).toContain('after');
		expect(frame).not.toContain('');
	});

	test('renders bold inline markdown inside image alt text', () => {
		const {lastFrame} = render(
			<Markdown source={'![**important** logo](https://x/i.png)'} />,
		);
		const frame = lastFrame() ?? '';
		// The asterisks must be consumed (parsed as bold), not shown literally.
		expect(frame).toContain('important');
		expect(frame).toContain('logo');
		expect(frame).not.toContain('**');
	});

	test('renders task list items with checkbox glyphs and no raw [x]', () => {
		const {lastFrame} = render(
			<Markdown source={'- [x] done\n- [ ] todo'} />,
		);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('☒');
		expect(frame).toContain('☐');
		expect(frame).toContain('done');
		expect(frame).toContain('todo');
		expect(frame).not.toContain('[x]');
		expect(frame).not.toContain('[ ]');
	});

	test('loose task lists also strip the checkbox token (not just tight lists)', () => {
		// Loose lists wrap each item's prose in a paragraph whose inline
		// tokens lead with a checkbox — a separate code path from the tight
		// case above. Both must elide the raw `[x] ` / `[ ] `.
		const {lastFrame} = render(
			<Markdown source={'- [x] one\n\n- [ ] two'} />,
		);
		const frame = lastFrame() ?? '';
		expect(frame).toContain('☒');
		expect(frame).toContain('☐');
		expect(frame).toContain('one');
		expect(frame).toContain('two');
		expect(frame).not.toContain('[x]');
		expect(frame).not.toContain('[ ]');
	});

	test('whitespace-only source does not crash and renders a single inert row', () => {
		const {lastFrame} = render(<Markdown source={'   \n\t\n'} />);
		const frame = lastFrame() ?? '';
		// Should not throw; output should be at most a small placeholder.
		expect(frame).not.toContain('undefined');
		expect(frame.length).toBeLessThan(10);
	});
});

// ink-testing-library renders in Ink's debug mode, which strips ANSI from
// frames, and chalk's color level varies by environment. So we verify the
// integration by calling cli-highlight with the Solarized theme directly:
// the output must contain ANSI escapes and must visibly differ from the
// plain input — proving the theme is wired through cli-highlight.
describe('Solarized syntax theme via cli-highlight', () => {
	test('paints python code (escapes are emitted around tokens)', () => {
		const source = 'def f():\n    return 1';
		const out = highlight(source, {
			language: 'python',
			theme: solarizedSyntaxTheme,
		});
		expect(out).toMatch(ANSI_ESCAPE);
		expect(out).not.toBe(source);
		expect(out).toContain('def');
		expect(out).toContain('return');
	});

	test('paints javascript code', () => {
		const source = 'const s = "hi"';
		const out = highlight(source, {
			language: 'javascript',
			theme: solarizedSyntaxTheme,
		});
		expect(out).toMatch(ANSI_ESCAPE);
		expect(out).not.toBe(source);
	});

	test('theme map covers the highlight.js classes we depend on', () => {
		// Spot-check: missing any of these would silently drop color on common
		// tokens, which is the failure mode the user reported.
		const expected = [
			'keyword',
			'string',
			'comment',
			'number',
			'function',
			'title',
			'built_in',
			'type',
			'literal',
		];
		for (const slot of expected) {
			expect(typeof (solarizedSyntaxTheme as Record<string, unknown>)[slot]).toBe(
				'function',
			);
		}
	});
});
