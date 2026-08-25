#!/usr/bin/env python3
"""Standalone nirdosha codegen test client — no ProtoBox pipeline machinery,
no structured output. System prompt = scratch/nirdosha_llm_prompt.md verbatim.
User prompt = one real UserStory from Neo4j, attached as text. Prints the
LLM's raw response, unparsed."""
import json
import sys
from pathlib import Path

import httpx

sys.path.insert(0, "/home/arun/Downloads/protobox/be-v2/src")
from dotenv import load_dotenv
load_dotenv("/home/arun/Downloads/protobox/be-v2/.env")

from core.graph.repository import list_entities_by_project

SYSTEM_PROMPT = Path("/home/arun/Downloads/nirdosha/scratch/nirdosha_llm_prompt.md").read_text()
BASE_URL = "http://localhost:11434/v1"
MODEL = "deepseek-v4-flash:0731-cloud"


def main():
    project_id = sys.argv[1] if len(sys.argv) > 1 else "trade-finance-b2b-2"
    story_index = int(sys.argv[2]) if len(sys.argv) > 2 else 0

    stories = list_entities_by_project("UserStory", project_id, limit=2000)
    story = stories[story_index]
    story_json = json.dumps(story, indent=2, default=str)

    user_prompt = (
        "=== USER STORY JSON ===\n"
        f"{story_json}\n"
        "=== END USER STORY JSON ===\n\n"
        "Give me the nirdosha code for this story."
    )

    print(f"=== STORY ({project_id}[{story_index}]) ===", story.get("title") or story.get("name"), file=sys.stderr)
    print("=== CALLING LLM (raw completion, no structured output) ===", file=sys.stderr)

    resp = httpx.post(
        f"{BASE_URL}/chat/completions",
        headers={"Authorization": "Bearer ollama"},
        json={
            "model": MODEL,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": user_prompt},
            ],
        },
        timeout=600,
    )
    resp.raise_for_status()
    data = resp.json()
    content = data["choices"][0]["message"]["content"]
    print("=== RAW LLM RESPONSE ===")
    print(content)


if __name__ == "__main__":
    main()
