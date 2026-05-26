import chalk from 'chalk';
import type {Theme} from 'cli-highlight';

// Solarized's palette is defined at the truecolor (24-bit) level — every
// downgrade to 256/16 colors loses the perceptual relationships that make the
// scheme work. Force a level-3 chalk instance so the hex values emit
// `38;2;R;G;B` regardless of how chalk auto-detects the current stdout (which
// in non-TTY contexts like test runners is otherwise level 0).
const c = new chalk.Instance({level: 3});

const yellow = c.hex('#b58900');
const orange = c.hex('#cb4b16');
const red = c.hex('#dc322f');
const magenta = c.hex('#d33682');
const violet = c.hex('#6c71c4');
const blue = c.hex('#268bd2');
const cyan = c.hex('#2aa198');
const green = c.hex('#859900');
const base01 = c.hex('#586e75');
const base0 = c.hex('#839496');

export const solarizedSyntaxTheme: Theme = {
	keyword: green,
	built_in: violet,
	type: yellow,
	class: yellow,
	literal: magenta,
	number: magenta,
	regexp: cyan,
	string: cyan,
	subst: base0,
	symbol: orange,
	function: blue,
	title: blue,
	params: base0,
	comment: base01,
	doctag: base01,
	meta: orange,
	'meta-keyword': orange,
	'meta-string': cyan,
	section: blue,
	tag: blue,
	name: blue,
	'builtin-name': violet,
	attr: yellow,
	attribute: yellow,
	variable: base0,
	bullet: orange,
	code: cyan,
	emphasis: c.italic,
	strong: c.bold,
	formula: cyan,
	link: blue,
	quote: base01,
	'selector-tag': green,
	'selector-id': blue,
	'selector-class': yellow,
	'selector-attr': yellow,
	'selector-pseudo': violet,
	'template-tag': blue,
	'template-variable': cyan,
	addition: green,
	deletion: red,
	default: base0,
};
