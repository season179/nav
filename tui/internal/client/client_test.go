package client

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"os/exec"
	"strings"
	"testing"
)

func TestClientSendMessageUsesJSONRPCAndParsesAssistantDeltas(t *testing.T) {
	const (
		sessionID      = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
		runID          = "019f2f6f-f179-7a72-9f28-7f9aa0a1c853"
		messageID      = "019f2f6f-f17a-7a72-9f28-7f9aa0a1c853"
		sessionEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"
		deltaEventID   = "019f2f6f-f17c-7a72-9f28-7f9aa0a1c853"
		doneEventID    = "019f2f6f-f17d-7a72-9f28-7f9aa0a1c853"
	)

	var rpcMethods []string
	var sendMessageText string
	var sawLastEventID string

	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			rpcMethods = append(rpcMethods, request.Method)

			switch request.Method {
			case "session.create":
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result":  map[string]any{"sessionId": sessionID},
				}), nil
			case "session.sendMessage":
				params := request.Params.(map[string]any)
				if params["sessionId"] != sessionID {
					t.Fatalf("session.sendMessage used session %v, want %s", params["sessionId"], sessionID)
				}
				sendMessageText = params["text"].(string)
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result": map[string]any{
						"sessionId": sessionID,
						"runId":     runID,
						"messageId": messageID,
					},
				}), nil
			default:
				t.Fatalf("unexpected JSON-RPC method %q", request.Method)
			}
		case r.Method == http.MethodGet && r.URL.Path == "/sessions/"+sessionID+"/events":
			if len(rpcMethods) == 1 {
				return sseResponse(
					sseEventText(sessionEventID, "session.created", map[string]any{
						"event_id":   sessionEventID,
						"session_id": sessionID,
						"type":       "session.created",
					}),
				), nil
			}

			sawLastEventID = r.Header.Get("Last-Event-ID")
			return sseResponse(
				sseEventText(deltaEventID, "model.text_delta", map[string]any{
					"event_id":   deltaEventID,
					"session_id": sessionID,
					"type":       "model.text_delta",
					"run_id":     runID,
					"message_id": messageID,
					"delta":      "hello from backend",
					"metadata": map[string]any{
						"provider_id":         "compatible-gateway",
						"configured_model_id": "vendor/model-large",
					},
				}),
				sseEventText(doneEventID, "run.completed", map[string]any{
					"event_id":   doneEventID,
					"session_id": sessionID,
					"type":       "run.completed",
					"run_id":     runID,
				}),
			), nil
		default:
			t.Fatalf("unexpected request %s %s", r.Method, r.URL.Path)
		}
		return nil, nil
	})

	client := NewWithEndpoint("http://backend.test")
	client.httpClient = &http.Client{Transport: transport}
	if _, err := client.Connect(context.Background()); err != nil {
		t.Fatalf("connect client: %v", err)
	}

	events, err := client.SendMessage(context.Background(), "hello backend")
	if err != nil {
		t.Fatalf("send message: %v", err)
	}

	if sendMessageText != "hello backend" {
		t.Fatalf("sent text %q, want %q", sendMessageText, "hello backend")
	}
	if sawLastEventID != sessionEventID {
		t.Fatalf("Last-Event-ID = %q, want %q", sawLastEventID, sessionEventID)
	}
	if len(events) != 2 {
		t.Fatalf("events len = %d, want 2", len(events))
	}
	if events[0].Type != "model.text_delta" || events[0].Delta != "hello from backend" {
		t.Fatalf("first event = %#v, want assistant text delta", events[0])
	}
	if events[1].Type != "run.completed" {
		t.Fatalf("second event = %#v, want run.completed", events[1])
	}
}

func TestClientCloseClearsOwnedBackendSessionState(t *testing.T) {
	client := NewWithEndpoint("http://backend.test")
	client.cmd = &exec.Cmd{}
	client.session = SessionInfo{
		SessionID: "019f2f6f-f178-7a72-9f28-7f9aa0a1c853",
		Endpoint:  "http://backend.test",
		CWD:       "/tmp/nav",
	}
	client.lastEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"

	if err := client.Close(); err != nil {
		t.Fatalf("close client: %v", err)
	}

	if client.cmd != nil {
		t.Fatal("owned backend command was not cleared")
	}
	if client.session != (SessionInfo{}) {
		t.Fatalf("session = %#v, want empty session", client.session)
	}
	if client.endpoint != "" || client.lastEventID != "" {
		t.Fatalf("endpoint/lastEventID = %q/%q, want cleared", client.endpoint, client.lastEventID)
	}
}

func TestClientCloseClearsExternalEndpointSessionState(t *testing.T) {
	client := NewWithEndpoint("http://backend.test")
	client.session = SessionInfo{
		SessionID: "019f2f6f-f178-7a72-9f28-7f9aa0a1c853",
		Endpoint:  "http://backend.test",
		CWD:       "/tmp/nav",
	}
	client.lastEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"

	if err := client.Close(); err != nil {
		t.Fatalf("close client: %v", err)
	}

	if client.endpoint != "http://backend.test" {
		t.Fatalf("endpoint = %q, want external endpoint preserved", client.endpoint)
	}
	if client.session != (SessionInfo{}) {
		t.Fatalf("session = %#v, want empty session", client.session)
	}
	if client.lastEventID != "" {
		t.Fatalf("lastEventID = %q, want cleared", client.lastEventID)
	}
}

func TestClientConnectClearsPartialSessionWhenInitialEventsFail(t *testing.T) {
	const sessionID = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			return jsonResponse(t, map[string]any{
				"jsonrpc": "2.0",
				"id":      request.ID,
				"result":  map[string]any{"sessionId": sessionID},
			}), nil
		case r.Method == http.MethodGet && r.URL.Path == "/sessions/"+sessionID+"/events":
			return response(http.StatusInternalServerError, "text/plain", "event stream unavailable"), nil
		default:
			t.Fatalf("unexpected request %s %s", r.Method, r.URL.Path)
		}
		return nil, nil
	})

	client := NewWithEndpoint("http://backend.test")
	client.httpClient = &http.Client{Transport: transport}

	if _, err := client.Connect(context.Background()); err == nil {
		t.Fatal("Connect succeeded, want initial event fetch error")
	}
	if client.session != (SessionInfo{}) {
		t.Fatalf("session = %#v, want cleared partial session", client.session)
	}
	if client.lastEventID != "" {
		t.Fatalf("lastEventID = %q, want cleared", client.lastEventID)
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (fn roundTripFunc) RoundTrip(request *http.Request) (*http.Response, error) {
	return fn(request)
}

func jsonResponse(t *testing.T, value any) *http.Response {
	t.Helper()
	body, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("marshal JSON response: %v", err)
	}
	return response(http.StatusOK, "application/json", string(body))
}

func sseResponse(events ...string) *http.Response {
	return response(http.StatusOK, "text/event-stream", strings.Join(events, ""))
}

func sseEventText(id string, event string, data any) string {
	body, _ := json.Marshal(data)
	return "id: " + id + "\n" +
		"event: " + event + "\n" +
		"data: " + string(body) + "\n\n"
}

func response(status int, contentType string, body string) *http.Response {
	return &http.Response{
		StatusCode: status,
		Header:     http.Header{"Content-Type": []string{contentType}},
		Body:       io.NopCloser(strings.NewReader(body)),
	}
}
