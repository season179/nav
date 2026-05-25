# Model Provider Settings

`nav-harness` resolves model/provider settings without making HTTP requests.
The first supported API shape is OpenAI-compatible Chat Completions.

The config shape intentionally follows Pi's `models.json` closely: `providers`
is keyed by provider id, each provider owns its endpoint settings, and each
provider lists the models available behind that endpoint.

Ready-to-use example configs live in
`crates/nav-harness/examples/model-settings-*.json`.

Prefer environment variables for credentials. String values follow Pi's
behavior: nav checks the environment first, then falls back to the string as a
literal value.

```json
{
  "defaultModel": {
    "provider": "local-gateway",
    "model": "local-coder:7b"
  },
  "providers": {
    "local-gateway": {
      "name": "Local Gateway",
      "baseUrl": "http://localhost:11434/v1",
      "api": "openai-completions",
      "apiKey": "LOCAL_GATEWAY_API_KEY",
      "compat": {
        "supportsDeveloperRole": false,
        "supportsReasoningEffort": false,
        "supportsUsageInStreaming": true,
        "maxTokensField": "max_tokens"
      },
      "models": [
        {
          "id": "local-coder:7b",
          "name": "Local Coder 7B",
          "reasoning": true,
          "input": ["text"],
          "contextWindow": 128000,
          "maxTokens": 32000
        }
      ]
    }
  }
}
```

`baseUrl` is ordinary config, so OpenAI-compatible traffic can point at any
compatible endpoint, proxy, gateway, or self-hosted server:

```json
{
  "providers": {
    "team-proxy": {
      "name": "Team Proxy",
      "baseUrl": "https://llm.example.com/v1",
      "api": "openai-completions",
      "apiKey": "TEAM_PROXY_API_KEY",
      "models": [
        {
          "id": "vendor/model-large",
          "reasoning": true,
          "input": ["text", "image"]
        }
      ]
    }
  }
}
```

Inline credentials are allowed for local/private setups, but logs, errors, and
debug output must not reveal them:

```json
{
  "providers": {
    "private-local": {
      "baseUrl": "http://localhost:8080/v1",
      "api": "openai-completions",
      "apiKey": { "inline": "local-only-secret" },
      "models": [{ "id": "local-model" }]
    }
  }
}
```

If you want nav to fail clearly when an environment variable is missing, use the
explicit env-var form:

```json
{
  "apiKey": { "envVar": "TEAM_PROXY_API_KEY" }
}
```

For endpoints with non-default request quirks, set generic compatibility fields
at the provider level or override them on a model:

```json
{
  "providers": {
    "compatible-endpoint": {
      "baseUrl": "https://llm.example.com/v1",
      "api": "openai-completions",
      "apiKey": "COMPATIBLE_ENDPOINT_API_KEY",
      "compat": {
        "thinkingFormat": "qwen-chat-template",
        "supportsUsageInStreaming": false,
        "maxTokensField": "max_tokens"
      },
      "models": [
        {
          "id": "vendor/model-with-overrides",
          "compat": {
            "thinkingFormat": "openai"
          }
        }
      ]
    }
  }
}
```
