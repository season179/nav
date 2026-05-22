#!/usr/bin/env python3
"""Parse a Chat Completions SSE file into an expected-output sidecar dict.

Usage: parse_sse.py <file.sse> <description>

Writes JSON to stdout describing what the accumulator should produce.
"""

import json
import sys


def parse(path: str) -> dict:
    assistant_text: list[str] = []
    tool_calls: dict[int, dict] = {}
    finish_reason = None
    usage = None
    error = None

    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line.startswith("data:"):
                continue
            payload = line[len("data:"):].strip()
            if payload in ("[DONE]", ""):
                continue
            try:
                ev = json.loads(payload)
            except json.JSONDecodeError:
                continue

            # Error shape (no choices array)
            if "error" in ev and "choices" not in ev:
                err = ev["error"]
                error = {
                    "type": err.get("code", err.get("type", "unknown")),
                    "message": err.get("message", ""),
                }
                continue

            choices = ev.get("choices", [])
            if not choices:
                usage = ev.get("usage")
                continue

            choice = choices[0]
            delta = choice.get("delta", {})

            if choice.get("finish_reason") is not None:
                finish_reason = choice["finish_reason"]

            content = delta.get("content")
            if content:
                assistant_text.append(content)

            for tc in delta.get("tool_calls", []):
                idx = tc.get("index", 0)
                if idx not in tool_calls:
                    tool_calls[idx] = {
                        "index": idx,
                        "id": tc.get("id", ""),
                        "type": tc.get("type", "function"),
                        "function": {
                            "name": tc.get("function", {}).get("name", ""),
                            "arguments": "",
                        },
                    }
                fn = tc.get("function", {})
                if fn.get("name"):
                    tool_calls[idx]["function"]["name"] = fn["name"]
                if fn.get("arguments"):
                    tool_calls[idx]["function"]["arguments"] += fn["arguments"]

            ev_usage = ev.get("usage")
            if ev_usage:
                usage = ev_usage

    joined = "".join(assistant_text) or None
    return {
        "assistant_text": None if error else joined,
        "tool_calls": [tool_calls[k] for k in sorted(tool_calls)],
        "finish_reason": finish_reason,
        "usage": usage,
        **({"error": error} if error else {}),
    }


def main() -> None:
    path, description = sys.argv[1], sys.argv[2]
    result = {"description": description, "expected": parse(path)}
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
