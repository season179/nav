package client

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const (
	rpcSessionCreate      = "session.create"
	rpcSessionSendMessage = "session.sendMessage"
)

var errSSEStreamEndedBeforeExpectedEvent = errors.New("SSE stream ended before the expected event")

type SessionInfo struct {
	SessionID string
	Endpoint  string
	CWD       string
}

type Event struct {
	ID        string
	Type      string
	SessionID string
	RunID     string
	MessageID string
	Delta     string
	Text      string
	Message   string
}

type Client struct {
	mu          sync.Mutex
	backendPath string
	endpoint    string
	httpClient  *http.Client
	cmd         *exec.Cmd
	session     SessionInfo
	lastEventID string
}

func New() *Client {
	return NewWithBackendPath("")
}

func NewWithBackendPath(backendPath string) *Client {
	return &Client{backendPath: backendPath, httpClient: http.DefaultClient}
}

func NewWithEndpoint(endpoint string) *Client {
	return &Client{endpoint: strings.TrimRight(endpoint, "/"), httpClient: http.DefaultClient}
}

func (c *Client) Connect(ctx context.Context) (SessionInfo, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	return c.connectLocked(ctx)
}

func (c *Client) SendMessage(ctx context.Context, text string) ([]Event, error) {
	events, errs := c.StreamMessage(ctx, text)
	var collected []Event
	for event := range events {
		collected = append(collected, event)
	}
	if err, ok := <-errs; ok && err != nil {
		return nil, err
	}
	return collected, nil
}

func (c *Client) StreamMessage(ctx context.Context, text string) (<-chan Event, <-chan error) {
	events := make(chan Event, 16)
	errs := make(chan error, 1)

	go func() {
		defer close(events)
		defer close(errs)
		if err := c.streamMessage(ctx, text, events); err != nil {
			errs <- err
		}
	}()

	return events, errs
}

func (c *Client) streamMessage(ctx context.Context, text string, events chan<- Event) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if _, err := c.connectLocked(ctx); err != nil {
		return err
	}
	if strings.TrimSpace(text) == "" {
		return errors.New("message text is required")
	}

	result, err := c.callRPCLocked(ctx, rpcSessionSendMessage, map[string]any{
		"sessionId": c.session.SessionID,
		"text":      text,
	})
	if err != nil {
		return err
	}

	var send struct {
		RunID string `json:"runId"`
	}
	if err := json.Unmarshal(result, &send); err != nil {
		return fmt.Errorf("decode session.sendMessage result: %w", err)
	}
	if send.RunID == "" {
		return errors.New("session.sendMessage returned an empty run id")
	}

	return c.streamEventsLocked(ctx, runTerminalEvent(send.RunID), events)
}

func (c *Client) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if c.cmd == nil {
		c.session = SessionInfo{}
		c.lastEventID = ""
		return nil
	}

	c.stopOwnedBackendLocked()
	return nil
}

func (c *Client) stopOwnedBackendLocked() {
	cmd := c.cmd
	c.cmd = nil
	c.endpoint = ""
	c.session = SessionInfo{}
	c.lastEventID = ""

	if cmd.Process != nil {
		_ = cmd.Process.Kill()
	}
	_ = cmd.Wait()
}

func (c *Client) connectLocked(ctx context.Context) (SessionInfo, error) {
	if c.session.SessionID != "" {
		return c.session, nil
	}

	if err := c.startLocked(ctx); err != nil {
		return SessionInfo{}, err
	}

	cwd, _ := os.Getwd()
	result, err := c.callRPCLocked(ctx, rpcSessionCreate, map[string]any{
		"cwd":    cwd,
		"source": "tui",
	})
	if err != nil {
		return SessionInfo{}, err
	}

	var create struct {
		SessionID string `json:"sessionId"`
	}
	if err := json.Unmarshal(result, &create); err != nil {
		return SessionInfo{}, fmt.Errorf("decode session.create result: %w", err)
	}
	if create.SessionID == "" {
		return SessionInfo{}, errors.New("session.create returned an empty session id")
	}

	c.session = SessionInfo{
		SessionID: create.SessionID,
		Endpoint:  c.endpoint,
		CWD:       cwd,
	}
	if _, err := c.fetchEventsLocked(ctx, sessionCreatedEvent); err != nil {
		c.session = SessionInfo{}
		c.lastEventID = ""
		return SessionInfo{}, err
	}

	return c.session, nil
}

func (c *Client) startLocked(ctx context.Context) error {
	if c.endpoint != "" {
		return nil
	}
	if c.cmd != nil {
		return nil
	}

	cmd, err := backendCommand(ctx, c.backendPath)
	if err != nil {
		return err
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
	scanner := bufio.NewScanner(stdout)
	if !scanner.Scan() {
		c.stopOwnedBackendLocked()
		if err := scanner.Err(); err != nil {
			return fmt.Errorf("read backend bootstrap: %w", err)
		}
		return errors.New("backend exited without bootstrap endpoint")
	}

	var ready backendReady
	if err := json.Unmarshal(scanner.Bytes(), &ready); err != nil {
		c.stopOwnedBackendLocked()
		return fmt.Errorf("decode backend bootstrap: %w", err)
	}
	if ready.Type != "backend.ready" || ready.BaseURL == "" {
		c.stopOwnedBackendLocked()
		return fmt.Errorf("unexpected backend bootstrap: %s", scanner.Text())
	}
	c.endpoint = strings.TrimRight(ready.BaseURL, "/")
	return nil
}

func (c *Client) callRPCLocked(ctx context.Context, method string, params any) (json.RawMessage, error) {
	if c.endpoint == "" {
		return nil, errors.New("backend endpoint is not available")
	}

	requestID, err := newUUIDv7()
	if err != nil {
		return nil, err
	}
	payload, err := json.Marshal(jsonRPCRequest{
		JSONRPC: "2.0",
		ID:      requestID,
		Method:  method,
		Params:  params,
	})
	if err != nil {
		return nil, fmt.Errorf("encode JSON-RPC request: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.endpoint+"/rpc", bytes.NewReader(payload))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("post JSON-RPC %s: %w", method, err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("read JSON-RPC response: %w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("JSON-RPC %s returned HTTP %d: %s", method, resp.StatusCode, strings.TrimSpace(string(body)))
	}

	var response jsonRPCResponse
	if err := json.Unmarshal(body, &response); err != nil {
		return nil, fmt.Errorf("decode JSON-RPC response: %w", err)
	}
	if response.Error != nil {
		return nil, errors.New(response.Error.Message)
	}
	if len(response.Result) == 0 {
		return nil, fmt.Errorf("JSON-RPC %s returned no result", method)
	}
	return response.Result, nil
}

func (c *Client) fetchEventsLocked(ctx context.Context, stop func(Event) bool) ([]Event, error) {
	var events []Event
	done, err := c.readSessionEventsLocked(ctx, stop, func(event Event) error {
		events = append(events, event)
		return nil
	})
	if err != nil {
		return nil, err
	}
	if stop != nil && !done {
		return nil, errSSEStreamEndedBeforeExpectedEvent
	}
	return events, nil
}

func (c *Client) streamEventsLocked(ctx context.Context, stop func(Event) bool, events chan<- Event) error {
	for {
		lastEventID := c.lastEventID
		done, err := c.readSessionEventsLocked(ctx, stop, func(event Event) error {
			select {
			case events <- event:
				return nil
			case <-ctx.Done():
				return ctx.Err()
			}
		})
		if err != nil {
			if shouldReconnectAfterSSEReadError(ctx, err, c.lastEventID != lastEventID) {
				continue
			}
			return err
		}
		if done {
			return nil
		}
		if c.lastEventID == lastEventID {
			return errSSEStreamEndedBeforeExpectedEvent
		}
	}
}

func (c *Client) readSessionEventsLocked(ctx context.Context, stop func(Event) bool, emit func(Event) error) (bool, error) {
	if c.session.SessionID == "" {
		return false, errors.New("session is not connected")
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.endpoint+"/sessions/"+c.session.SessionID+"/events", nil)
	if err != nil {
		return false, err
	}
	if c.lastEventID != "" {
		req.Header.Set("Last-Event-ID", c.lastEventID)
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return false, fmt.Errorf("get session events: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return false, fmt.Errorf("session events returned HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}

	return scanSSE(resp.Body, func(event Event) (bool, error) {
		if event.ID != "" {
			c.lastEventID = event.ID
		}
		if err := emit(event); err != nil {
			return false, err
		}
		return stop != nil && stop(event), nil
	})
}

type jsonRPCRequest struct {
	JSONRPC string `json:"jsonrpc"`
	ID      string `json:"id"`
	Method  string `json:"method"`
	Params  any    `json:"params,omitempty"`
}

type jsonRPCResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      string          `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *jsonRPCError   `json:"error,omitempty"`
}

type jsonRPCError struct {
	Code    int64  `json:"code"`
	Message string `json:"message"`
}

type backendReady struct {
	Type    string `json:"type"`
	BaseURL string `json:"baseUrl"`
}

type eventPayload struct {
	EventID   string `json:"event_id"`
	SessionID string `json:"session_id"`
	Type      string `json:"type"`
	RunID     string `json:"run_id"`
	MessageID string `json:"message_id"`
	Delta     string `json:"delta"`
	Text      string `json:"text"`
	Message   string `json:"message"`
}

func parseSSE(reader io.Reader) ([]Event, error) {
	return parseSSEUntil(reader, nil)
}

func parseSSEUntil(reader io.Reader, stop func(Event) bool) ([]Event, error) {
	var events []Event
	done, err := scanSSE(reader, func(event Event) (bool, error) {
		events = append(events, event)
		return stop != nil && stop(event), nil
	})
	if err != nil {
		return nil, err
	}
	if stop != nil && !done {
		return nil, errSSEStreamEndedBeforeExpectedEvent
	}
	return events, nil
}

func scanSSE(reader io.Reader, emit func(Event) (bool, error)) (bool, error) {
	scanner := bufio.NewScanner(reader)
	scanner.Buffer(make([]byte, 1024), 1024*1024)

	current := Event{}
	var dataLines []string
	flush := func() (bool, error) {
		if current.ID == "" && current.Type == "" && len(dataLines) == 0 {
			return false, nil
		}
		event, err := decodeSSEEvent(current, dataLines)
		if err != nil {
			return false, err
		}
		current = Event{}
		dataLines = nil
		return emit(event)
	}

	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			done, err := flush()
			if err != nil {
				return false, err
			}
			if done {
				return true, nil
			}
			continue
		}

		switch {
		case strings.HasPrefix(line, "id:"):
			current.ID = strings.TrimSpace(strings.TrimPrefix(line, "id:"))
		case strings.HasPrefix(line, "event:"):
			current.Type = strings.TrimSpace(strings.TrimPrefix(line, "event:"))
		case strings.HasPrefix(line, "data:"):
			dataLines = append(dataLines, strings.TrimSpace(strings.TrimPrefix(line, "data:")))
		}
	}
	if err := scanner.Err(); err != nil {
		return false, &sseReadError{err: err}
	}
	return flush()
}

type sseReadError struct {
	err error
}

func (err *sseReadError) Error() string {
	return fmt.Sprintf("read SSE stream: %v", err.err)
}

func (err *sseReadError) Unwrap() error {
	return err.err
}

func shouldReconnectAfterSSEReadError(ctx context.Context, err error, madeProgress bool) bool {
	if ctx.Err() != nil || !madeProgress {
		return false
	}

	var readErr *sseReadError
	return errors.As(err, &readErr)
}

func sessionCreatedEvent(event Event) bool {
	return event.Type == "session.created"
}

func runTerminalEvent(runID string) func(Event) bool {
	return func(event Event) bool {
		if event.RunID != runID {
			return false
		}
		switch event.Type {
		case "run.completed", "run.failed", "run.cancelled":
			return true
		default:
			return false
		}
	}
}

func decodeSSEEvent(event Event, dataLines []string) (Event, error) {
	var payload eventPayload
	if len(dataLines) > 0 {
		if err := json.Unmarshal([]byte(strings.Join(dataLines, "\n")), &payload); err != nil {
			return Event{}, fmt.Errorf("decode SSE event %q: %w", event.Type, err)
		}
	}

	if event.Type == "" {
		event.Type = payload.Type
	}
	if event.ID == "" {
		event.ID = payload.EventID
	}
	event.SessionID = payload.SessionID
	event.RunID = payload.RunID
	event.MessageID = payload.MessageID
	event.Delta = payload.Delta
	event.Text = payload.Text
	event.Message = payload.Message
	return event, nil
}

func backendCommand(ctx context.Context, backendPath string) (*exec.Cmd, error) {
	if backendPath != "" {
		return exec.CommandContext(ctx, backendPath, "serve-http"), nil
	}

	if path := os.Getenv("NAV_BACKEND"); path != "" {
		return exec.CommandContext(ctx, path, "serve-http"), nil
	}

	if exe, err := os.Executable(); err == nil {
		sibling := filepath.Join(filepath.Dir(exe), "nav-backend")
		if isExecutable(sibling) {
			return exec.CommandContext(ctx, sibling, "serve-http"), nil
		}
	}

	if manifest := findWorkspaceManifest(); manifest != "" {
		return exec.CommandContext(ctx, "cargo", "run", "--quiet", "--manifest-path", manifest, "-p", "nav-backend", "--", "serve-http"), nil
	}

	return exec.CommandContext(ctx, "nav-backend", "serve-http"), nil
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

func newUUIDv7() (string, error) {
	var bytes [16]byte
	if _, err := rand.Read(bytes[6:]); err != nil {
		return "", fmt.Errorf("generate request id: %w", err)
	}

	millis := uint64(time.Now().UnixMilli())
	bytes[0] = byte(millis >> 40)
	bytes[1] = byte(millis >> 32)
	bytes[2] = byte(millis >> 24)
	bytes[3] = byte(millis >> 16)
	bytes[4] = byte(millis >> 8)
	bytes[5] = byte(millis)
	bytes[6] = (bytes[6] & 0x0f) | 0x70
	bytes[8] = (bytes[8] & 0x3f) | 0x80

	return fmt.Sprintf("%x-%x-%x-%x-%x", bytes[0:4], bytes[4:6], bytes[6:8], bytes[8:10], bytes[10:16]), nil
}
