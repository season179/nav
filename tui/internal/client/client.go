package client

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
)

type Request struct {
	Type string `json:"type"`
	CWD  string `json:"cwd,omitempty"`
}

type Response struct {
	Type    string `json:"type"`
	Name    string `json:"name,omitempty"`
	Version string `json:"version,omitempty"`
	CWD     string `json:"cwd,omitempty"`
	Message string `json:"message,omitempty"`
}

type Client struct {
	mu          sync.Mutex
	backendPath string
	cmd         *exec.Cmd
	stdin       io.WriteCloser
	stdout      *bufio.Scanner
}

func New() *Client {
	return NewWithBackendPath("")
}

func NewWithBackendPath(backendPath string) *Client {
	return &Client{backendPath: backendPath}
}

func (c *Client) Hello(ctx context.Context) (Response, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if err := c.start(ctx); err != nil {
		return Response{}, err
	}

	cwd, _ := os.Getwd()
	return c.send(Request{Type: "hello", CWD: cwd})
}

func (c *Client) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if c.cmd == nil {
		return nil
	}

	_, _ = c.send(Request{Type: "shutdown"})
	err := c.cmd.Wait()
	c.cmd = nil
	return err
}

func (c *Client) start(ctx context.Context) error {
	if c.cmd != nil {
		return nil
	}

	cmd, err := backendCommand(ctx, c.backendPath)
	if err != nil {
		return err
	}

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("open backend stdin: %w", err)
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("open backend stdout: %w", err)
	}

	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("start backend: %w", err)
	}

	c.cmd = cmd
	c.stdin = stdin
	c.stdout = bufio.NewScanner(stdout)
	return nil
}

func (c *Client) send(request Request) (Response, error) {
	if c.stdin == nil || c.stdout == nil {
		return Response{}, errors.New("backend is not running")
	}

	if err := json.NewEncoder(c.stdin).Encode(request); err != nil {
		return Response{}, fmt.Errorf("write backend request: %w", err)
	}

	if !c.stdout.Scan() {
		if err := c.stdout.Err(); err != nil {
			return Response{}, fmt.Errorf("read backend response: %w", err)
		}
		return Response{}, errors.New("backend exited without a response")
	}

	var response Response
	if err := json.Unmarshal(c.stdout.Bytes(), &response); err != nil {
		return Response{}, fmt.Errorf("decode backend response: %w", err)
	}
	if response.Type == "error" {
		return Response{}, errors.New(response.Message)
	}
	return response, nil
}

func backendCommand(ctx context.Context, backendPath string) (*exec.Cmd, error) {
	if backendPath != "" {
		return exec.CommandContext(ctx, backendPath, "serve"), nil
	}

	if path := os.Getenv("NAV_BACKEND"); path != "" {
		return exec.CommandContext(ctx, path, "serve"), nil
	}

	if exe, err := os.Executable(); err == nil {
		sibling := filepath.Join(filepath.Dir(exe), "nav-backend")
		if isExecutable(sibling) {
			return exec.CommandContext(ctx, sibling, "serve"), nil
		}
	}

	if manifest := findWorkspaceManifest(); manifest != "" {
		return exec.CommandContext(ctx, "cargo", "run", "--quiet", "--manifest-path", manifest, "-p", "nav-backend", "--", "serve"), nil
	}

	return exec.CommandContext(ctx, "nav-backend", "serve"), nil
}

func findWorkspaceManifest() string {
	dir, err := os.Getwd()
	if err != nil {
		return ""
	}

	for {
		manifest := filepath.Join(dir, "Cargo.toml")
		if data, err := os.ReadFile(manifest); err == nil && strings.Contains(string(data), "nav-backend") {
			return manifest
		}

		parent := filepath.Dir(dir)
		if parent == dir {
			return ""
		}
		dir = parent
	}
}

func isExecutable(path string) bool {
	info, err := os.Stat(path)
	return err == nil && !info.IsDir() && info.Mode()&0111 != 0
}
