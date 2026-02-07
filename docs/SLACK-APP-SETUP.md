# Slack App Setup

## Overview

Kleviathan uses a Slack app for three runtime behaviors:

- verify token scopes with `auth.test`
- set bot presence with `users.setPresence`
- resolve email addresses to Slack user IDs with `users.lookupByEmail`, falling back to `users.list` when Slack returns `users_not_found`
- send messages with `chat.postMessage`

The connector does not read Slack message history, update messages, or delete messages.

## Create a Slack App

1. Go to [https://api.slack.com/apps](https://api.slack.com/apps).
2. Click **Create New App**.
3. Choose **From scratch**.
4. Name the app and select the workspace.
5. Create the app.

## Configure Bot Token Scopes

Kleviathan currently verifies that the Slack token has this exact set of 14 bot scopes:

| Scope | Why the current implementation expects it |
| --- | --- |
| `channels:history` | Required by exact scope verification |
| `channels:join` | Required by exact scope verification |
| `channels:read` | Required by exact scope verification |
| `chat:write` | Required for `chat.postMessage` |
| `groups:history` | Required by exact scope verification |
| `groups:read` | Required by exact scope verification |
| `im:history` | Required by exact scope verification |
| `im:read` | Required by exact scope verification |
| `im:write` | Required by exact scope verification |
| `incoming-webhook` | Required by exact scope verification |
| `search:read.users` | Required by exact scope verification |
| `users:read` | Required for `users.list` fallback handling |
| `users:read.email` | Required for email-based user lookup |
| `users:write` | Required for `users.setPresence` |

If any scope is missing or any extra scope is present, startup fails.

## Install to Workspace

1. Open **OAuth & Permissions**.
2. Click **Install to Workspace** or **Reinstall to Workspace**.
3. Approve the requested scopes.
4. Copy the **Bot User OAuth Token** that starts with `xoxb-`.

## Configure Kleviathan

Add the token to the `slack` section of `~/.kleviathan/kleviathan.jsonc`:

```jsonc
{
  "slack": {
    "bot_token": "xoxb-your-token-here"
  }
}
```

There is no `default_channel` field in the current config schema.

## Enforcement

Kleviathan enforces Slack behavior in two ways.

Compile-time source scan:

- allowed endpoints present in production code:
  - `auth.test`
  - `users.setPresence`
  - `users.lookupByEmail`
  - `users.list`
  - `chat.postMessage`
- banned endpoints asserted absent from production code:
  - `conversations.history`
  - `conversations.replies`
  - `channels.history`
  - `im.history`
  - `chat.update`
  - `chat.delete`

Runtime verification:

- `verify_scopes()` calls `auth.test` and compares the returned `x-oauth-scopes` header against the exact required set
- `set_presence()` calls `users.setPresence`
- `lookup_user_by_email()` calls `users.lookupByEmail` and falls back to paginated `users.list`
- all handled Slack HTTP 429 responses are mapped to `KleviathanError::RateLimit`
