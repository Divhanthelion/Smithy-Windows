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

Requires an API key from a model provider (Anthropic, OpenRouter, DeepSeek,
OpenAI and others; OpenRouter has free models) or a model server of your own,
and Git for Windows, a free download, for the agent to run commands. Usage is
billed by your provider.

Uses live generative AI: the agent's replies are written by the third-party
model you choose, and can be wrong. Every reply has a report button.

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
- Report any AI reply from the reply itself
- No account, no subscription, no telemetry
- Open source

## Search terms (up to 7)

code editor, AI agent, coding assistant, OpenRouter, DeepSeek, Rust, developer tools

## Notes for certification

Paste into Submission → Submission options → Notes for certification, with a
real key in place of the brackets (Store Policies 10.3.1: a tester must be
able to try the agent). Make a separate OpenRouter key for this with a credit
limit of a few dollars (openrouter.ai → Keys → Create, set *Credit limit*),
and delete it once the app is certified.

> Smithy is a code editor with an AI agent. The agent needs a model
> provider key; here is one for testing, limited to a few dollars of usage:
> [OPENROUTER KEY]
>
> 1. Launch Smithy. On first launch it opens a folder named "Smithy
>    Projects" in the user folder and shows the setup dialog (later: Agent →
>    Backend Settings).
> 2. Choose OpenRouter, paste the key, keep the model shown or pick
>    deepseek/deepseek-chat, and press Save.
> 3. In the agent panel on the right, ask: "Create hello.txt saying hello,
>    then list the files in this project." Approve the change when the diff
>    appears.
>
> The editor, file browser and terminal panel work without a key. The agent
> runs commands through Git for Windows (https://git-scm.com/download/win);
> without it Smithy says so, and the agent can still read and edit files.
>
> Live generative AI: replies come from the third-party model the user
> selects. Each reply has a "report" button, and the Agent menu has "Report
> an AI Reply…"; both open
> https://github.com/Divhanthelion/Smithy-Windows/issues/new?template=ai-content.yml
>
> runFullTrust: Smithy edits project folders anywhere on disk, runs the
> user's builds and tests, and starts a language server (rust-analyzer) when
> one is installed.
