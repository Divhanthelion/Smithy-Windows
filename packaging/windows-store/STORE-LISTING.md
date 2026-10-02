# Store listing text

Paste into Partner Center → Store listings → English. Limits: description
10,000 characters, short description 1,000, each feature 200, up to 20
features.

## Product name

Smithy

## Short description

A fast, lightweight code editor with an AI agent that uses the model you
choose. Bring your own API key (Claude, OpenAI, OpenRouter, DeepSeek, Groq, Mistral,
xAI and more) or point it at your own model server. Your keys stay in Windows Credential Manager.

## Description

Smithy is a code editor built around an AI coding agent that you control.

You choose the model. Paste an API key for Claude (Anthropic), OpenRouter (hundreds of models,
including free ones), DeepSeek, or any service with an OpenAI-compatible API:
OpenAI, Groq, Mistral, xAI, Together, Fireworks, Gemini, or one you name. Or
point Smithy at a model server you run yourself, such as LM Studio or vLLM. Keys are kept in Windows Credential
Manager and sent only to the provider they belong to. Smithy has no account,
no subscription and no telemetry.

The agent reads your project, searches it, edits files and runs your builds
and tests. You decide how much it may do on its own: review each change as a
diff before it lands, or let it work and check the result. An optional second
model, Jev, watches the agent for risky commands, for going in circles, and
for claiming to be done when it isn't.

Smithy is written in Rust and stays small: the editor itself uses a fraction
of the memory of browser-based editors, which leaves more for your tools and
local models.

Also included: syntax highlighting and language-server support, a project
map and call graph for Rust code, a terminal panel, web search for the agent
(with your own Brave Search key), and dictation into the agent's prompt that
is recognised on your computer, never uploaded.

Requirements: the agent runs commands through Git for Windows, a free
download (Smithy tells you if it is missing). AI features need an API key
from a model provider or a model server of your own; usage is billed by that
provider.

Smithy is open source: https://github.com/Divhanthelion/Smithy-Windows

## Features

- Bring your own model: Claude, OpenAI, OpenRouter, DeepSeek, Groq, Mistral, xAI, Gemini, or your own server
- Choose your own Jev: TypeSafe, the Vercel AI Gateway, or a model on your own GPU
- API keys kept in Windows Credential Manager, sent only to their provider
- An AI agent that reads, searches, edits, builds and tests your project
- Review every change as a diff, or let the agent work on its own
- Jev, an optional second model that checks the agent's work
- Lightweight native app written in Rust
- Language-server support and a project map and call graph for Rust code
- On-device dictation into the agent's prompt
- No account, no subscription, no telemetry
- Open source

## Search terms (up to 7)

code editor, AI agent, coding assistant, OpenRouter, DeepSeek, Rust, developer tools

## Notes for certification

Smithy is a code editor. To try the agent, testers need an API key from
OpenRouter (free models are available) or DeepSeek; without one, the editor,
file browser and terminal work and the agent shows a setup dialog on first
launch. Running commands requires Git for Windows to be installed.
