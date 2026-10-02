# Smithy privacy policy

*Effective 1 October 2026. Applies to the Smithy editor and `smithy-agent`,
including the Microsoft Store version.*

**Smithy collects nothing about you.** It has no account, no telemetry, no
analytics and no crash reporting, and its developers receive no data from it.
Everything below is about where *your own* data goes when you use a feature
that talks to a service you chose.

## What stays on your computer

- Your projects. Smithy reads and edits files in the folders you open.
- Settings, conversations and Run logs, in `%USERPROFILE%\.local\share\smithy`
  (on macOS and Linux, `~/.local/share/smithy`).
- API keys you enter, in Windows Credential Manager (on macOS, the Keychain).
  They are never written to a settings file, never shown again after you save
  them, and sent only to the service they belong to.
- Dictation. Speech is recognised on your computer; audio is never uploaded.

## What is sent, and to whom

Only when you use the feature, and only to the service you configured:

| Feature | Sent to | What is sent |
|---|---|---|
| The agent | The model provider you chose (OpenRouter, DeepSeek, an OpenAI-compatible service such as OpenAI or Groq, or your own server) | Your messages, and what the agent reads to answer them: parts of your project's files, command output, search results. This is the provider's data under its own privacy policy. |
| Jev checks (optional) | The Jev service you chose: TypeSafe directly, TypeSafe through the Vercel AI Gateway, or a server you name | Short excerpts: your request, the last few tool calls and results, a proposed shell command, an answer; in an unattended Run, task titles, test output and research notes. Never a whole conversation or a whole file. |
| Web search (optional) | Brave Search | The search query the agent writes. |
| Web fetch | The website at the address | An ordinary request for that page. |
| MCP servers (optional) | Servers you configure in your project | What those tools are asked to do. |
| Dictation, first use | GitHub (`github.com/k2-fsa/sherpa-onnx` releases) | A download of the speech model (464 MB). Nothing about you or your audio. |

Each of those services handles what it receives under its own terms and
privacy policy. Requests name the app, not you: OpenRouter receives Smithy's
name and project address (its standard app-attribution headers), and fetched
web pages see the user agent `Smithy/<version> (+agent)`. Smithy adds no
identifier for you or your computer to any request.

## Children

Smithy is a developer tool and is not directed at children.

## Changes and contact

Changes to this policy are made in this file, with the history visible in
the repository. Questions: open an issue at
https://github.com/Divhanthelion/Smithy-Windows/issues.
