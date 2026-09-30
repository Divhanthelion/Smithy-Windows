#!/usr/bin/env python3
"""Flash-Next speed on fixed prompts: time to first token, decode tok/s, cached tokens.

Each prompt is sent twice; the second shows what prefix caching saves. Stdlib only.
usage: bench.py [label] [--url http://localhost:8000] [--model qwen3.8-flash-next]
"""
import json, sys, time, urllib.request

args = sys.argv[1:]
def opt(name, default):
    if name in args:
        i = args.index(name); v = args[i + 1]; del args[i:i + 2]; return v
    return default
URL = opt("--url", "http://localhost:8000")
MODEL = opt("--model", "qwen3.8-flash-next")
STREAMS = int(opt("--streams", "1"))
LABEL = args[0] if args else "run"

# A long shared prefix, as an agent's system prompt and project map would be.
PREFIX = "You are a careful Rust engineer working in a large codebase.\n" + "\n".join(
    f"src/module_{i}.rs: pub fn handler_{i}(input: &str) -> Result<Output{i}, Error> — parses and validates request kind {i}"
    for i in range(400)
)
PROMPTS = {
    "code": "Write a Rust function that parses ISO 8601 durations like P3Y6M4DT12H30M5S into a struct, with unit tests. Code only.",
    "prose": "Explain, in about 300 words, why prefix caching speeds up an agent loop that resends its whole history each turn.",
    "json": 'Return a JSON array of 12 objects {"name","language","year","paradigm"} for well-known programming languages. JSON only.',
}

def once(prompt, max_tokens=400):
    body = json.dumps({
        "model": MODEL, "stream": True, "max_tokens": max_tokens, "temperature": 0,
        "stream_options": {"include_usage": True},
        "chat_template_kwargs": {"enable_thinking": False},
        "messages": [{"role": "system", "content": PREFIX}, {"role": "user", "content": prompt}],
    }).encode()
    req = urllib.request.Request(URL + "/v1/chat/completions", body, {"Content-Type": "application/json"})
    t0 = time.time(); first = None; usage = {}
    with urllib.request.urlopen(req, timeout=600) as r:
        for line in r:
            line = line.decode().strip()
            if not line.startswith("data: ") or line == "data: [DONE]":
                continue
            ev = json.loads(line[6:])
            if ev.get("usage"):
                usage = ev["usage"]
            for c in ev.get("choices", []):
                if first is None and (c.get("delta", {}).get("content") or c.get("delta", {}).get("reasoning_content")):
                    first = time.time()
    t1 = time.time()
    out = usage.get("completion_tokens", 0)
    cached = (usage.get("prompt_tokens_details") or {}).get("cached_tokens")
    ttft = (first or t1) - t0
    decode = out / max(t1 - (first or t0), 1e-6)
    return {"prompt": usage.get("prompt_tokens"), "cached": cached, "out": out,
            "ttft_s": round(ttft, 2), "decode_tps": round(decode, 1)}

results = {"label": LABEL, "at": time.strftime("%Y-%m-%d %H:%M:%S"), "runs": {}}
if STREAMS == 1:
    for name, p in PROMPTS.items():
        results["runs"][name] = [once(p), once(p)]
        a, b = results["runs"][name]
        print(f"{LABEL:>10} {name:>5}: cold ttft {a['ttft_s']}s  warm ttft {b['ttft_s']}s  "
              f"cached {b['cached']}/{b['prompt']}  decode {a['decode_tps']}/{b['decode_tps']} tok/s", flush=True)
else:
    # Every prompt at once, STREAMS copies each round-robin: what an agent
    # running several independent Sessions would send. Warm the prefix first.
    import threading
    once(PROMPTS["json"], 8)
    jobs = [list(PROMPTS.values())[i % len(PROMPTS)] for i in range(STREAMS)]
    out = [None] * STREAMS
    def run(i):
        out[i] = once(jobs[i])
    t0 = time.time()
    threads = [threading.Thread(target=run, args=(i,)) for i in range(STREAMS)]
    for t in threads: t.start()
    for t in threads: t.join()
    wall = time.time() - t0
    total = sum(o["out"] for o in out)
    results["runs"]["parallel"] = out
    per = ", ".join(f"{o['decode_tps']}" for o in out)
    print(f"{LABEL:>10} {STREAMS} streams: per-stream decode {per} tok/s; "
          f"{total} tokens in {wall:.1f}s = {total / wall:.1f} tok/s aggregate", flush=True)
with open(f"bench-{LABEL}.json", "w") as f:
    json.dump(results, f, indent=1)
