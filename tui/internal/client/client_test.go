package client

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os/exec"
	"strings"
	"testing"
	"time"
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
	var createSource string
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
				params := request.Params.(map[string]any)
				createSource = params["source"].(string)
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
	if createSource != "tui" {
		t.Fatalf("session.create source = %q, want tui", createSource)
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

func TestClientStreamMessageEmitsDelayedChunksBeforeRunCompletes(t *testing.T) {
	const (
		sessionID      = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
		runID          = "019f2f6f-f179-7a72-9f28-7f9aa0a1c853"
		messageID      = "019f2f6f-f17a-7a72-9f28-7f9aa0a1c853"
		sessionEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"
		deltaEventID   = "019f2f6f-f17c-7a72-9f28-7f9aa0a1c853"
		doneEventID    = "019f2f6f-f17d-7a72-9f28-7f9aa0a1c853"
	)

	completeRun := make(chan struct{})
	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			switch request.Method {
			case "session.create":
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result":  map[string]any{"sessionId": sessionID},
				}), nil
			case "session.sendMessage":
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
			if r.Header.Get("Last-Event-ID") == "" {
				return sseResponse(
					sseEventText(sessionEventID, "session.created", map[string]any{
						"event_id":   sessionEventID,
						"session_id": sessionID,
						"type":       "session.created",
					}),
				), nil
			}

			return delayedSSEResponse(completeRun,
				sseEventText(deltaEventID, "model.text_delta", map[string]any{
					"event_id":   deltaEventID,
					"session_id": sessionID,
					"type":       "model.text_delta",
					"run_id":     runID,
					"message_id": messageID,
					"delta":      "streamed before completion",
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

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()

	client := NewWithEndpoint("http://backend.test")
	client.httpClient = &http.Client{Transport: transport}
	if _, err := client.Connect(ctx); err != nil {
		t.Fatalf("connect client: %v", err)
	}

	events, errs := client.StreamMessage(ctx, "hello backend")
	first := receiveEvent(t, events)
	if first.Type != "model.text_delta" || first.Delta != "streamed before completion" {
		t.Fatalf("first streamed event = %#v, want delayed model text delta", first)
	}

	close(completeRun)
	second := receiveEvent(t, events)
	if second.Type != "run.completed" {
		t.Fatalf("second streamed event = %#v, want run.completed", second)
	}
	if err := receiveStreamError(t, errs); err != nil {
		t.Fatalf("stream error: %v", err)
	}
}

func TestClientStreamMessageReconnectsWithLastEventIDAfterDroppedSSE(t *testing.T) {
	const (
		sessionID      = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
		runID          = "019f2f6f-f179-7a72-9f28-7f9aa0a1c853"
		messageID      = "019f2f6f-f17a-7a72-9f28-7f9aa0a1c853"
		sessionEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"
		deltaEventID   = "019f2f6f-f17c-7a72-9f28-7f9aa0a1c853"
		doneEventID    = "019f2f6f-f17d-7a72-9f28-7f9aa0a1c853"
	)

	streamRequests := 0
	var reconnectLastEventID string
	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			switch request.Method {
			case "session.create":
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result":  map[string]any{"sessionId": sessionID},
				}), nil
			case "session.sendMessage":
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
			if r.Header.Get("Last-Event-ID") == "" {
				return sseResponse(
					sseEventText(sessionEventID, "session.created", map[string]any{
						"event_id":   sessionEventID,
						"session_id": sessionID,
						"type":       "session.created",
					}),
				), nil
			}

			streamRequests++
			switch streamRequests {
			case 1:
				return sseResponse(
					sseEventText(deltaEventID, "model.text_delta", map[string]any{
						"event_id":   deltaEventID,
						"session_id": sessionID,
						"type":       "model.text_delta",
						"run_id":     runID,
						"message_id": messageID,
						"delta":      "before reconnect",
					}),
				), nil
			case 2:
				reconnectLastEventID = r.Header.Get("Last-Event-ID")
				return sseResponse(
					sseEventText(doneEventID, "run.completed", map[string]any{
						"event_id":   doneEventID,
						"session_id": sessionID,
						"type":       "run.completed",
						"run_id":     runID,
					}),
				), nil
			default:
				t.Fatalf("unexpected reconnect request %d", streamRequests)
			}
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

	events, errs := client.StreamMessage(context.Background(), "hello backend")
	first := receiveEvent(t, events)
	second := receiveEvent(t, events)
	if first.Type != "model.text_delta" || first.Delta != "before reconnect" {
		t.Fatalf("first event = %#v, want delta before reconnect", first)
	}
	if second.Type != "run.completed" {
		t.Fatalf("second event = %#v, want run.completed after reconnect", second)
	}
	if reconnectLastEventID != deltaEventID {
		t.Fatalf("reconnect Last-Event-ID = %q, want %q", reconnectLastEventID, deltaEventID)
	}
	if err := receiveStreamError(t, errs); err != nil {
		t.Fatalf("stream error: %v", err)
	}
}

func TestClientStreamMessageReconnectsWithLastEventIDAfterSSEReadError(t *testing.T) {
	const (
		sessionID      = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
		runID          = "019f2f6f-f179-7a72-9f28-7f9aa0a1c853"
		messageID      = "019f2f6f-f17a-7a72-9f28-7f9aa0a1c853"
		sessionEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"
		deltaEventID   = "019f2f6f-f17c-7a72-9f28-7f9aa0a1c853"
		doneEventID    = "019f2f6f-f17d-7a72-9f28-7f9aa0a1c853"
	)

	streamRequests := 0
	var reconnectLastEventID string
	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			switch request.Method {
			case "session.create":
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result":  map[string]any{"sessionId": sessionID},
				}), nil
			case "session.sendMessage":
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
			if r.Header.Get("Last-Event-ID") == "" {
				return sseResponse(
					sseEventText(sessionEventID, "session.created", map[string]any{
						"event_id":   sessionEventID,
						"session_id": sessionID,
						"type":       "session.created",
					}),
				), nil
			}

			streamRequests++
			switch streamRequests {
			case 1:
				return sseReadErrorResponse(
					sseEventText(deltaEventID, "model.text_delta", map[string]any{
						"event_id":   deltaEventID,
						"session_id": sessionID,
						"type":       "model.text_delta",
						"run_id":     runID,
						"message_id": messageID,
						"delta":      "before read error",
					}),
				), nil
			case 2:
				reconnectLastEventID = r.Header.Get("Last-Event-ID")
				return sseResponse(
					sseEventText(doneEventID, "run.completed", map[string]any{
						"event_id":   doneEventID,
						"session_id": sessionID,
						"type":       "run.completed",
						"run_id":     runID,
					}),
				), nil
			default:
				t.Fatalf("unexpected reconnect request %d", streamRequests)
			}
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

	events, errs := client.StreamMessage(context.Background(), "hello backend")
	first := receiveEvent(t, events)
	second := receiveEvent(t, events)
	if first.Type != "model.text_delta" || first.Delta != "before read error" {
		t.Fatalf("first event = %#v, want delta before read error", first)
	}
	if second.Type != "run.completed" {
		t.Fatalf("second event = %#v, want run.completed after reconnect", second)
	}
	if reconnectLastEventID != deltaEventID {
		t.Fatalf("reconnect Last-Event-ID = %q, want %q", reconnectLastEventID, deltaEventID)
	}
	if err := receiveStreamError(t, errs); err != nil {
		t.Fatalf("stream error: %v", err)
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

func TestClientStopsReadingLiveSSEAfterExpectedEvents(t *testing.T) {
	const (
		sessionID      = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
		runID          = "019f2f6f-f179-7a72-9f28-7f9aa0a1c853"
		messageID      = "019f2f6f-f17a-7a72-9f28-7f9aa0a1c853"
		sessionEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"
		deltaEventID   = "019f2f6f-f17c-7a72-9f28-7f9aa0a1c853"
		doneEventID    = "019f2f6f-f17d-7a72-9f28-7f9aa0a1c853"
	)

	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			switch request.Method {
			case "session.create":
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result":  map[string]any{"sessionId": sessionID},
				}), nil
			case "session.sendMessage":
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
			if r.Header.Get("Last-Event-ID") == "" {
				return liveSSEResponse(
					sseEventText(sessionEventID, "session.created", map[string]any{
						"event_id":   sessionEventID,
						"session_id": sessionID,
						"type":       "session.created",
					}),
				), nil
			}
			return liveSSEResponse(
				sseEventText(deltaEventID, "model.text_delta", map[string]any{
					"event_id":   deltaEventID,
					"session_id": sessionID,
					"type":       "model.text_delta",
					"run_id":     runID,
					"message_id": messageID,
					"delta":      "hello from live stream",
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

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()

	client := NewWithEndpoint("http://backend.test")
	client.httpClient = &http.Client{Transport: transport}
	if _, err := client.Connect(ctx); err != nil {
		t.Fatalf("connect client: %v", err)
	}

	events, err := client.SendMessage(ctx, "hello backend")
	if err != nil {
		t.Fatalf("send message: %v", err)
	}
	if len(events) != 2 {
		t.Fatalf("events len = %d, want 2", len(events))
	}
	if events[0].Delta != "hello from live stream" || events[1].Type != "run.completed" {
		t.Fatalf("events = %#v, want live delta then run.completed", events)
	}
}

func TestClientErrorsWhenLiveSSEEndsBeforeExpectedEvent(t *testing.T) {
	const (
		sessionID      = "019f2f6f-f178-7a72-9f28-7f9aa0a1c853"
		runID          = "019f2f6f-f179-7a72-9f28-7f9aa0a1c853"
		messageID      = "019f2f6f-f17a-7a72-9f28-7f9aa0a1c853"
		sessionEventID = "019f2f6f-f17b-7a72-9f28-7f9aa0a1c853"
		deltaEventID   = "019f2f6f-f17c-7a72-9f28-7f9aa0a1c853"
	)

	transport := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/rpc":
			var request jsonRPCRequest
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Fatalf("decode JSON-RPC request: %v", err)
			}
			switch request.Method {
			case "session.create":
				return jsonResponse(t, map[string]any{
					"jsonrpc": "2.0",
					"id":      request.ID,
					"result":  map[string]any{"sessionId": sessionID},
				}), nil
			case "session.sendMessage":
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
			if r.Header.Get("Last-Event-ID") == "" {
				return sseResponse(
					sseEventText(sessionEventID, "session.created", map[string]any{
						"event_id":   sessionEventID,
						"session_id": sessionID,
						"type":       "session.created",
					}),
				), nil
			}
			return sseResponse(
				sseEventText(deltaEventID, "model.text_delta", map[string]any{
					"event_id":   deltaEventID,
					"session_id": sessionID,
					"type":       "model.text_delta",
					"run_id":     runID,
					"message_id": messageID,
					"delta":      "partial response",
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

	_, err := client.SendMessage(context.Background(), "hello backend")
	if !errors.Is(err, errSSEStreamEndedBeforeExpectedEvent) {
		t.Fatalf("SendMessage error = %v, want early live SSE close", err)
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

func liveSSEResponse(events ...string) *http.Response {
	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"text/event-stream"}},
		Body:       &openEndedBody{reader: strings.NewReader(strings.Join(events, ""))},
	}
}

func delayedSSEResponse(release <-chan struct{}, first string, rest ...string) *http.Response {
	reader, writer := io.Pipe()
	go func() {
		_, _ = io.WriteString(writer, first)
		<-release
		for _, event := range rest {
			_, _ = io.WriteString(writer, event)
		}
		_ = writer.Close()
	}()

	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"text/event-stream"}},
		Body:       reader,
	}
}

func sseReadErrorResponse(events ...string) *http.Response {
	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"text/event-stream"}},
		Body: &errorAfterBody{
			reader: strings.NewReader(strings.Join(events, "")),
			err:    errors.New("SSE connection dropped"),
		},
	}
}

var errLiveStreamStillOpen = errors.New("live SSE stream is still open")

type openEndedBody struct {
	reader *strings.Reader
}

func (body *openEndedBody) Read(p []byte) (int, error) {
	if body.reader.Len() > 0 {
		return body.reader.Read(p)
	}
	return 0, errLiveStreamStillOpen
}

func (body *openEndedBody) Close() error {
	return nil
}

type errorAfterBody struct {
	reader *strings.Reader
	err    error
}

func (body *errorAfterBody) Read(p []byte) (int, error) {
	if body.reader.Len() > 0 {
		return body.reader.Read(p)
	}
	return 0, body.err
}

func (body *errorAfterBody) Close() error {
	return nil
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

func receiveEvent(t *testing.T, events <-chan Event) Event {
	t.Helper()
	select {
	case event, ok := <-events:
		if !ok {
			t.Fatal("event stream closed before expected event")
		}
		return event
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for streamed event")
	}
	return Event{}
}

func receiveStreamError(t *testing.T, errs <-chan error) error {
	t.Helper()
	select {
	case err, ok := <-errs:
		if !ok {
			return nil
		}
		return err
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for stream completion")
	}
	return nil
}
