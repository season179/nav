package localdev

import (
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

func Run(args []string, sourceRoot string) int {
	if len(args) > 0 {
		switch args[0] {
		case "update":
			if err := update(args[1:], sourceRoot); err != nil {
				fmt.Fprintf(os.Stderr, "navd update failed: %v\n", err)
				return 1
			}
			return 0
		case "help", "-h", "--help":
			printHelp()
			return 0
		}
	}

	root, err := resolveSourceRoot(sourceRoot)
	if err != nil {
		fmt.Fprintf(os.Stderr, "navd cannot find its source checkout: %v\n", err)
		fmt.Fprintln(os.Stderr, "Run `navd update` from the nav checkout to refresh the development launcher.")
		return 1
	}

	if err := runLocalNav(root, args); err != nil {
		fmt.Fprintf(os.Stderr, "navd failed: %v\n", err)
		return 1
	}
	return 0
}

func runLocalNav(root string, args []string) error {
	navPath := filepath.Join(root, "target", "debug", "nav")
	if !isExecutable(navPath) {
		fmt.Fprintf(os.Stderr, "local nav is not built at %s\n", navPath)
		fmt.Fprintln(os.Stderr, "Run `navd update` from the nav checkout.")
		return errors.New("local nav binary missing")
	}

	backendPath := filepath.Join(root, "target", "debug", "nav-backend")
	if !isExecutable(backendPath) {
		fmt.Fprintf(os.Stderr, "local backend is not built at %s\n", backendPath)
		fmt.Fprintln(os.Stderr, "Run `navd update` from the nav checkout.")
		return errors.New("local backend binary missing")
	}

	cmd := exec.Command(navPath, args...)
	cmd.Env = append(os.Environ(), "NAV_BACKEND="+backendPath)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func printHelp() {
	fmt.Println(`navd runs the local development build of nav.

Usage:
  navd [args...]  Run target/debug/nav with target/debug/nav-backend
  navd update     Build local development binaries and install the launcher

Options for update:
  --install-dir <dir>  Install navd into this directory (default: ~/.local/bin)
  --no-install         Build target/debug/navd without installing it`)
}

type updateOptions struct {
	installDir string
	noInstall  bool
	help       bool
}

func parseUpdateOptions(args []string) (updateOptions, error) {
	opts := updateOptions{}
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--install-dir":
			if i+1 >= len(args) {
				return opts, errors.New("--install-dir requires a path")
			}
			opts.installDir = args[i+1]
			i++
		case "--no-install":
			opts.noInstall = true
		case "-h", "--help":
			opts.help = true
		default:
			return opts, fmt.Errorf("unknown option: %s", args[i])
		}
	}
	return opts, nil
}

func update(args []string, sourceRoot string) error {
	opts, err := parseUpdateOptions(args)
	if err != nil {
		return err
	}
	if opts.help {
		printHelp()
		return nil
	}

	root, err := resolveSourceRoot(sourceRoot)
	if err != nil {
		return err
	}

	targetDir := filepath.Join(root, "target", "debug")
	devBinary := filepath.Join(targetDir, "navd")
	localNavBinary := filepath.Join(targetDir, "nav")

	steps := []struct {
		name string
		cmd  *exec.Cmd
	}{
		{
			name: "backend",
			cmd: exec.Command(
				"cargo",
				"build",
				"--manifest-path",
				filepath.Join(root, "Cargo.toml"),
				"-p",
				"nav-backend",
			),
		},
		{
			name: "local nav tui",
			cmd: goCommand(
				root,
				"go",
				"build",
				"-o",
				localNavBinary,
				"./cmd/nav",
			),
		},
		{
			name: "development launcher",
			cmd: goCommand(
				root,
				"go",
				"build",
				"-ldflags",
				"-X main.sourceRoot="+root,
				"-o",
				devBinary,
				"./cmd/navd",
			),
		},
	}

	for _, step := range steps {
		fmt.Printf("building %s...\n", step.name)
		if err := run(step.cmd); err != nil {
			return err
		}
	}

	if opts.noInstall {
		fmt.Printf("built %s\n", devBinary)
		return nil
	}

	installDir, err := defaultInstallDir(opts.installDir)
	if err != nil {
		return err
	}

	installed := filepath.Join(installDir, "navd")
	if err := installFile(devBinary, installed); err != nil {
		return err
	}
	fmt.Printf("installed %s\n", installed)
	return nil
}

func goCommand(root string, name string, args ...string) *exec.Cmd {
	cmd := exec.Command(name, args...)
	cmd.Dir = filepath.Join(root, "tui")
	cmd.Env = append(os.Environ(),
		"GOCACHE="+filepath.Join(root, ".cache", "go-build"),
		"GOMODCACHE="+filepath.Join(root, ".cache", "go-mod"),
	)
	return cmd
}

func run(cmd *exec.Cmd) error {
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func defaultInstallDir(override string) (string, error) {
	if override != "" {
		return filepath.Abs(override)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".local", "bin"), nil
}

func installFile(src string, dst string) error {
	if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
		return err
	}

	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()

	tmp := dst + ".tmp"
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o755)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		_ = out.Close()
		return err
	}
	if err := out.Close(); err != nil {
		return err
	}
	if err := os.Chmod(tmp, 0o755); err != nil {
		return err
	}
	return os.Rename(tmp, dst)
}

func resolveSourceRoot(sourceRoot string) (string, error) {
	if sourceRoot != "" && isSourceRoot(sourceRoot) {
		return filepath.Abs(sourceRoot)
	}

	cwd, err := os.Getwd()
	if err == nil {
		if root := findSourceRoot(cwd); root != "" {
			return root, nil
		}
	}

	if exe, err := os.Executable(); err == nil {
		if root := findSourceRoot(filepath.Dir(exe)); root != "" {
			return root, nil
		}
	}

	return "", errors.New("no nav checkout found")
}

func findSourceRoot(start string) string {
	dir, err := filepath.Abs(start)
	if err != nil {
		return ""
	}
	for {
		if isSourceRoot(dir) {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return ""
		}
		dir = parent
	}
}

func isSourceRoot(dir string) bool {
	manifest := filepath.Join(dir, "Cargo.toml")
	data, err := os.ReadFile(manifest)
	if err != nil || !strings.Contains(string(data), "nav-backend") {
		return false
	}
	_, err = os.Stat(filepath.Join(dir, "tui", "go.mod"))
	return err == nil
}

func isExecutable(path string) bool {
	info, err := os.Stat(path)
	return err == nil && !info.IsDir() && info.Mode()&0111 != 0
}
