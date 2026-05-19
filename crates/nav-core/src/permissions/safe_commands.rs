//! Static safelist of read-only commands the model can run without prompting.
//!
//! Conservative on purpose: anything not on this list escalates to an
//! approval request under `AskForApproval::UnlessTrusted`. Subcommand-aware
//! entries (git/find/sed/cargo) live below the bare command list.

/// Commands whose argv[0] alone makes them auto-safe (no flag scrutiny).
///
/// `env` is intentionally **not** on this list: `env rm -rf build` would
/// otherwise be classified `Safe` because the classifier inspects argv[0]
/// rather than the wrapped command. Until we grow an env-aware parser that
/// can peel the leading `env [VAR=VAL]*` prefix and recurse, every `env`
/// invocation is treated as unknown and escalates under `UnlessTrusted`.
pub const SAFELIST_BARE: &[&str] = &[
    "cat", "cd", "cut", "date", "echo", "false", "file", "head", "id", "ls", "nl", "paste",
    "printf", "pwd", "rev", "rg", "seq", "sort", "stat", "tail", "tr", "tree", "true", "uname",
    "uniq", "wc", "which", "whoami",
];

/// `git` subcommands that are read-only. `config` and `branch` are
/// special-cased: `config --get` is safe but bare `config` mutates, and
/// `branch` is safe only with list-style flags (`branch tmp` writes under
/// `.git/refs`).
pub const GIT_READ_ONLY_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "remote",
    "rev-parse",
    "ls-files",
    "describe",
    "blame",
];

/// `cargo` subcommands that don't execute project code. **Not** a "read-only"
/// list in the casual sense: `cargo check`/`build`/`clippy` all compile and
/// run `build.rs` and proc macros from the workspace, which is real code
/// execution. They are therefore left to escalate; only commands that don't
/// touch project code go here.
pub const CARGO_READ_ONLY_SUBCOMMANDS: &[&str] = &[];

/// `find` flags that turn a read-only command into a side-effecting one.
pub const FIND_UNSAFE_FLAGS: &[&str] = &[
    "-exec", "-execdir", "-delete", "-fprint", "-fprintf", "-print0", "-ok", "-okdir",
];

/// Decide whether the given argv is auto-safe.
pub fn is_known_safe(argv: &[String]) -> bool {
    let Some(cmd) = argv.first().map(String::as_str) else {
        return false;
    };

    if SAFELIST_BARE.contains(&cmd) {
        // Special-case `find`: bare command is safe, but unsafe flags taint it.
        return true;
    }

    match cmd {
        "find" => find_is_safe(&argv[1..]),
        "git" => git_is_safe(&argv[1..]),
        "cargo" => cargo_is_safe(&argv[1..]),
        "grep" => grep_is_safe(&argv[1..]),
        "sed" => sed_is_safe(&argv[1..]),
        _ => false,
    }
}

fn find_is_safe(rest: &[String]) -> bool {
    !rest
        .iter()
        .any(|arg| FIND_UNSAFE_FLAGS.contains(&arg.as_str()))
}

fn git_is_safe(rest: &[String]) -> bool {
    let Some(sub) = rest.first().map(String::as_str) else {
        return false;
    };
    if GIT_READ_ONLY_SUBCOMMANDS.contains(&sub) {
        return true;
    }
    // `git config --get foo` is safe; bare `git config` mutates.
    if sub == "config" {
        return rest.iter().any(|arg| arg == "--get");
    }
    if sub == "branch" {
        return git_branch_is_read_only(&rest[1..]);
    }
    false
}

/// `git branch` is read-only when listing — i.e. the args contain no
/// positional branch name and no mutating flag (`-d`, `-D`, `-m`, `-M`,
/// `-c`, `-C`, `--edit-description`, `--set-upstream*`, etc.). When in
/// doubt, escalate; `git branch tmp` writes under `.git/refs`.
fn git_branch_is_read_only(args: &[String]) -> bool {
    // Listing flags that can appear without making the invocation mutating.
    const LIST_FLAGS: &[&str] = &[
        "-a",
        "--all",
        "-l",
        "--list",
        "-r",
        "--remotes",
        "-v",
        "-vv",
        "--verbose",
        "--show-current",
        "--no-color",
        "--column",
        "--no-column",
        "--merged",
        "--no-merged",
        "--contains",
        "--no-contains",
        "--points-at",
        "--format",
        "--sort",
        "-i",
        "--ignore-case",
    ];
    const LIST_FLAG_PREFIXES: &[&str] = &[
        "--sort=",
        "--contains=",
        "--no-contains=",
        "--merged=",
        "--no-merged=",
        "--column=",
        "--color=",
        "--format=",
        "--points-at=",
    ];

    args.iter().all(|arg| {
        LIST_FLAGS.contains(&arg.as_str())
            || LIST_FLAG_PREFIXES
                .iter()
                .any(|prefix| arg.starts_with(prefix))
    })
}

fn cargo_is_safe(rest: &[String]) -> bool {
    let Some(sub) = rest.first().map(String::as_str) else {
        return false;
    };
    if CARGO_READ_ONLY_SUBCOMMANDS.contains(&sub) {
        return true;
    }
    // `cargo fmt --check` is the only cargo command that doesn't invoke
    // the compiler over project code (so no `build.rs`/proc-macro
    // execution). Everything else escalates.
    if sub == "fmt" {
        return rest.iter().any(|arg| arg == "--check");
    }
    false
}

fn grep_is_safe(rest: &[String]) -> bool {
    // grep is read-only by nature; reject only the rare in-place flags
    // (`grep` itself doesn't have one, but `egrep -P` etc. are still reads).
    let _ = rest;
    true
}

fn sed_is_safe(rest: &[String]) -> bool {
    // Only allow `-n …p` style printing. Any `-i`/`--in-place` mutates.
    let mutates = rest.iter().any(|arg| {
        arg == "-i"
            || arg.starts_with("-i") && arg.len() > 2
            || arg == "--in-place"
            || arg.starts_with("--in-place")
    });
    let has_n = rest.iter().any(|arg| arg == "-n");
    !mutates && has_n
}
