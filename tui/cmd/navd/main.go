package main

import (
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"nav.local/tui/internal/app"
)

var sourceRoot string

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "update":
			if err := update(os.Args[2:]); err != nil {
				fmt.Fprintf(os.Stderr, "navd update failed: %v\n", err)
				os.Exit(1)
			}
			return
		case "help", "-h", "--help":
			printHelp()
			return
		default:
			fmt.Fprintf(os.Stderr, "unknown navd command: %s\n\n", os.Args[1])
			printHelp()
			os.Exit(2)
		}
	}

	root, err := resolveSourceRoot()
	if err != nil {
		fmt.Fprintf(os.Stderr, "navd cannot find its source checkout: %v\n", err)
		fmt.Fprintln(os.Stderr, "Run `navd update` from the nav checkout to refresh the dev launcher.")
		os.Exit(1)
	}

	backendPath := filepath.Join(root, "target", "debug", "nav-backend")
	if !isExecutable(backendPath) {
		fmt.Fprintf(os.Stderr, "navd backend is not built at %s\n", backendPath)
		fmt.Fprintln(os.Stderr, "Run `navd update` from the nav checkout.")
		os.Exit(1)
	}

	os.Exit(app.Run(backendPath))
}

func printHelp() {
	fmt.Println(`navd runs the local development build of nav.

Usage:
  navd          Run the locally built development TUI
  navd update   Build navd from the current checkout and install it

Options for update:
  --install-dir <dir>  Install navd into this directory (default: ~/.local/bin)
  --no-install         Build target/debug/navd without installing it`)
}

type updateOptions struct {
	installDir string
	noInstall  bool
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
			printHelp()
			os.Exit(0)
		default:
			return opts, fmt.Errorf("unknown option: %s", args[i])
		}
	}
	return opts, nil
}

func update(args []string) error {
	opts, err := parseUpdateOptions(args)
	if err != nil {
		return err
	}

	root, err := resolveSourceRoot()
	if err != nil {
		return err
	}

	targetDir := filepath.Join(root, "target", "debug")
	devBinary := filepath.Join(targetDir, "navd")
	prodBinary := filepath.Join(targetDir, "nav")

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
			name: "production tui",
			cmd: goCommand(
				root,
				"go",
				"build",
				"-o",
				prodBinary,
				"./cmd/nav",
			),
		},
		{
			name: "dev tui",
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

func resolveSourceRoot() (string, error) {
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
