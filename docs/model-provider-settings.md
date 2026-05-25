# Model Provider Settings

`nav-harness` resolves model/provider settings without making HTTP requests.
The first supported API shape is OpenAI-compatible Chat Completions.

Settings are shaped like Pi's `models.json`: `providers` is keyed by provider
id, and each provider owns the models served by that endpoint.

Prefer environment variables for credentials:

```json
{
  "default_model": "daily-driver",
  "providers": {
    "openrouter": {
      "display_name": "OpenRouter",
      "api_kind": "openai_chat_completions",
      "base_url": "https://openrouter.ai/api/v1",
      "api_key": { "env_var": "OPENROUTER_API_KEY" },
      "compat": {
        "thinking_format": "openrouter",
        "supports_usage_in_streaming": true,
        "max_tokens_field": "max_completion_tokens",
        "routing": {
          "only": ["anthropic"],
          "order": ["anthropic", "amazon-bedrock"]
        }
      },
      "models": [
        {
          "id": "daily-driver",
          "model_id": "openai/gpt-4.1",
          "capabilities": {
            "supports_tools": true,
            "supports_reasoning": true,
            "supports_images": false
          }
        }
      ]
    }
  }
}
```

`base_url` is ordinary config, so OpenAI-compatible traffic can point at any
compatible endpoint, proxy, gateway, or self-hosted server:

```json
{
  "providers": {
    "local-gateway": {
      "display_name": "Local Gateway",
      "api_kind": "openai_chat_completions",
      "base_url": "http://localhost:8787/openai/v1",
      "api_key": { "env_var": "LOCAL_GATEWAY_API_KEY" },
      "models": [
        {
          "id": "gateway-sonnet",
          "model_id": "anthropic/claude-sonnet-4"
        }
      ]
    }
  }
}
```

Z.ai-style providers use the same API kind and generic compatibility metadata:

```json
{
  "providers": {
    "zai": {
      "display_name": "Z.ai",
      "api_kind": "openai_chat_completions",
      "base_url": "http://localhost:8787/proxy/zai/v1",
      "api_key": { "env_var": "ZAI_API_KEY" },
      "compat": {
        "thinking_format": "zai",
        "supports_usage_in_streaming": false,
        "max_tokens_field": "max_tokens"
      },
      "models": [
        {
          "id": "glm",
          "model_id": "glm-4.5",
          "capabilities": {
            "supports_tools": true,
            "supports_reasoning": true,
            "supports_images": true
          }
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
  "api_key": { "inline": "local-only-secret" }
}
```
