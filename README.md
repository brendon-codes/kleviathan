# Kleviathan

An AI orchestrator that connects Matrix, Trello, Fastmail, and Slack through a safety-first execution pipeline.

## What is this?

Kleviathan is a personal AI assistant that runs inside Docker and talks to you over Matrix. You send it a message describing work to do across your connected tools, it produces a task graph, shows you the plan, and waits for confirmation before execution.

The system is intentionally narrow. Every incoming Matrix message is rate-limited, checked for abusive content, checked for injection payloads, and only then sent to the configured LLM. The LLM is never allowed to return code for execution. It must return schema-constrained JSON that maps onto a closed set of connector actions.

This is not a general-purpose agent framework. It is a constrained orchestration tool with hard limits around what each connector may do.

## Philosophy

Kleviathan is built from the constraint side first. The main question is not "what can the model do?" but "what is the smallest safe capability set that still solves useful cross-tool tasks?"

Defense in depth is the operating model. Static checks run before model-based checks. Connectors expose only pre-defined actions. The execution engine validates task graphs before running them. Execution fails fast on the first task error. Even if one layer behaves badly, the others are supposed to keep the blast radius small.

## Safety

### Rate Limiting

Every incoming message is checked against four independent limits:

- 4 per second
- 20 per minute
- 300 per hour
- 720 per day

All four checks run on every message. Hitting any limit rejects the message.

### Content Filtering

Abusive content detection is a two-pass pipeline:

- Static analysis with `rustrict`
- LLM verification with a strict JSON schema

Both passes must accept the message before planning continues.

### Injection Prevention

Injection detection also uses two passes:

- Static analysis with `libinjection` for SQLi and XSS
- LLM verification for actual embedded payload syntax such as command, template, and LDAP injection

The system blocks payloads before they can flow into downstream tools or stored content.

### Container Enforcement

`run-inner` refuses to start outside Docker unless you explicitly pass `--force-dangerous`. The container check looks for `/.dockerenv` and Docker/containerd markers in `/proc/1/cgroup`.

Kleviathan does persist operational state under `~/.kleviathan`:

- `kleviathan.jsonc` for configuration
- `logs/` for structured log files
- `matrix_store/` for the Matrix SQLite crypto store
- `matrix_session.json` for Matrix session restoration

### Connector Restrictions

Each connector is intentionally limited:

- **Matrix:** Only accepts non-empty messages from the configured `allowed_sender` in the resolved encrypted DM room. Outbound sends are capped by a 30-second timeout.
- **Trello:** Can create cards, search cards on a specific board, and fetch card details. There are no delete operations.
- **Fastmail JMAP (`fm_jmap`):** Read-only email access. Connector initialization rejects tokens with submission capability. Search windows cannot exceed 365 days.
- **Fastmail CalDAV (`fm_caldav`):** Can list calendars, search events, and create new events. It must not add invitees, edit existing events, delete events, or use PATCH.
- **Fastmail CardDAV (`fm_carddav`):** Can list address books, search contacts, and create new contacts. It must not edit existing contacts, delete contacts, or use PATCH.
- **Slack:** Can send messages and resolve users by email. It does not read message history or mutate existing messages.

### LLM Safety

The LLM is only used for planning, tool selection, parameter extraction, and final summarization. Every structured LLM response is validated against JSON schema with `additionalProperties: false`. Unknown tool or action pairs are rejected.

## Setup

### Prerequisites

- Docker
- A Matrix account on any homeserver
- An E2EE-capable Matrix client such as [Element](https://element.io/)
- Trello API credentials
- A Fastmail JMAP API key
- A Fastmail CalDAV app password
- A Fastmail CardDAV app password
- A Slack bot token
- An OpenAI or Anthropic API key matching your selected `llm.model`

### Configuration

Generate a config template:

```bash
kleviathan make-config
```

This creates `~/.kleviathan/kleviathan.jsonc`. The file is JSONC, so inline comments are allowed.

The current config schema requires every section shown in the bundled template.

The config sections are:

- `matrix`
  - `homeserver_url`
  - `username`
  - `password`
  - `allowed_sender`
  - `store_passphrase`
  - `enable_matrix_logs`
- `trello`
  - `api_key`
  - `token`
- `fm_jmap`
  - `api_key`
- `fm_caldav`
  - `username`
  - `password`
- `fm_carddav`
  - `username`
  - `password`
- `slack`
  - `bot_token`
- `llm`
  - `model`
  - `api_keys.openai`
  - `api_keys.anthropic`

The supported `llm.model` values are:

- `anthropic.sonnet46` -> `claude-sonnet-4-6`
- `anthropic.opus46` -> `claude-opus-4-6`
- `anthropic.haiku45` -> `claude-haiku-4-5-20251001`
- `openai.gpt52` -> `gpt-5.2`

The bundled template currently defaults to `anthropic.sonnet46`.

Connector-specific notes:

- `matrix.enable_matrix_logs` is required. `true` logs Matrix message bodies. `false` suppresses them from Matrix-specific logging.
- `fm_jmap` uses the Fastmail JMAP session endpoint compiled into the connector. You only configure the API key.
- `fm_caldav` and `fm_carddav` expect Fastmail app-password credentials and validate protocol isolation plus write access when those connectors are initialized.
- `slack` only accepts `bot_token`. There is no `default_channel` setting in the current config schema.

### Matrix Setup

1. Create a Matrix account for the bot.
2. Create an encrypted direct message room with the user who should control the bot.
3. Invite the bot account to that room.
4. Put the controlling Matrix user ID into `matrix.allowed_sender`.
5. Set `matrix.store_passphrase` and `matrix.enable_matrix_logs`.

The Matrix connector persists its encryption state in `~/.kleviathan/matrix_store/` and `~/.kleviathan/matrix_session.json` so restarts do not require a fresh login.

### Building and Running

Run from the repository root:

```bash
kleviathan run-container
```

This command:

1. Verifies `~/.kleviathan/kleviathan.jsonc` exists
2. Builds the Docker image
3. Stops any running `kleviathan` container
4. Starts a new container with `~/.kleviathan` mounted at `/home/kleviathan/.kleviathan`

Manual equivalent:

```bash
docker build -t kleviathan .

docker run \
  --rm \
  --name kleviathan \
  -v ~/.kleviathan:/home/kleviathan/.kleviathan \
  kleviathan run-inner
```

## Usage

Send a message to the bot in the configured Matrix room. Kleviathan:

1. Receives the Matrix event
2. Applies rate-limit, abuse, and injection checks
3. Uses the LLM to decompose the request into a task graph
4. Sends the formatted plan back to Matrix
5. Waits for `yes` or `y` to execute, or `no` or `n` to cancel
6. Executes tasks in dependency order and stops on the first failure
7. Sends a final result summary back to Matrix

When a connector action needs an explicit resource identifier such as a Trello list ID, Trello board ID, CalDAV calendar ID, or CardDAV address book ID, the plan must either contain that ID already or add a discovery step first.

### Examples

Create a Trello card in a known list:

```text
create a trello card in list 65f123abc456def789012345
title is "add last name to sign up form"
description is "the signup form needs a last name in addition to the first name"
```

Search email and then create a Trello card from the result:

```text
search emails from vendor@example.com for the last 15 days
then create a trello card in list 65f123abc456def789012345 using the email body
```

Search Trello cards on a known board and post the summary to Slack:

```text
search trello board 65f123abc456def789012346 for cards updated in the last 1 day
post the results into #tickets in slack
```

Search Fastmail calendar events after discovering calendars:

```text
list my calendars in fastmail
then search the first calendar for events between 2026-04-16 and 2026-04-23 with query "planning"
```

## Supported Integrations

| Integration | Direction | Notes |
| --- | --- | --- |
| Matrix | Input/Output | Primary interface. Requires an encrypted DM with the configured sender. |
| Trello | Limited read/write | `create_card`, `search_cards`, `get_card`. No deletes. |
| Fastmail JMAP | Read-only | `search_emails`, `get_email`. Submission capability rejected. |
| Fastmail CalDAV | Limited read/write | `list_calendars`, `search_events`, `add_event`. New events only. |
| Fastmail CardDAV | Limited read/write | `list_addressbooks`, `search_contacts`, `add_contact`. New contacts only. |
| Slack | Limited write/lookup | `send_message`, `lookup_user_by_email`. User lookup may fall back to `users.list`. |
| OpenAI | LLM provider | Enabled when `llm.model` is `openai.gpt52`. API model ID: `gpt-5.2`. |
| Anthropic | LLM provider | Enabled when `llm.model` is an `anthropic.*` variant. |
