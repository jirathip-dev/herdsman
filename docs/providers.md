# Providers and keys

herdsman supports two model providers: Anthropic (Claude Code lanes) and DeepSeek (dsh lanes, and
Hermes lanes that use DeepSeek).

**No secrets in fleet.toml, ever.** fleet.toml holds only the *names* of env variables. herdsman
checks every fleet.toml value on load. A value shaped like a token fails with `SECRET_IN_CONFIG`
(exit 3), and the refusal names the key, never the value. Token-shaped means a known key prefix
(`sk-`, GitHub and Slack token prefixes: the ones the page masks in pane text), a JWT, or a run of 32+ letters and
digits with no `.`/`/`/`-`/`_` in it. Env-var names (`CLAUDE_CODE_OAUTH_TOKEN_ACCOUNT_A1_PERSONAL`),
paths and model ids pass.

## Anthropic (Claude Code lanes)

**Lane tokens.** Each account's OAuth token lives in an env file. You point to it with `accounts_env`.

```toml
[accounts]
accounts_env = "~/.config/herdsman/accounts/tokens.env"   # CLAUDE_CODE_OAUTH_TOKEN_A=… lines, mode 0600
A = "CLAUDE_CODE_OAUTH_TOKEN_A"                          # a variable name, never the token
B = "CLAUDE_CODE_OAUTH_TOKEN_B"
```

Make a token with `claude setup-token` while logged in to that account. It is long-lived.
A role picks with `account = "A"` or an ordered `accounts = ["A", "B"]`. At spawn, the lane's
shell sources the file and exports the first uncapped account's token as `CLAUDE_CODE_OAUTH_TOKEN`.
The token is never copied, logged, or put in argv.

**Fallback and caps.** `herdsman check` reads each Claude lane's pane. When it shows the usage-limit
banner, it logs `account-capped`. Spawns then skip that account for `cap_reset_minutes` (default 300),
or until `herdsman accounts reset <name>`.

**Usage meter.** Setup tokens can't read the usage API: they lack the `user:profile` scope, and
`/api/oauth/usage` answers 403. To get the real meter, log each account in once with a full `claude`
login in its own config folder:

```sh
CLAUDE_CONFIG_DIR=~/.config/herdsman/accounts/A claude   # then /login as account A; repeat for B, C
```

```toml
[accounts.usage]
A = "~/.config/herdsman/accounts/A"   # no trailing slash: it must match CLAUDE_CONFIG_DIR exactly
```

On macOS, Claude Code keeps that login in the Keychain as a generic password. **The service name
is derived from Claude Code 2.1.308 and is unverified:** `Claude Code-credentials-<h>`, where `<h>`
is the first 8 hex digits of the SHA-256 of the `CLAUDE_CONFIG_DIR` string exactly as given. A
trailing slash, `~` or another spelling of the same folder gives a different hash, so write the path
in `[accounts.usage]` exactly as you set `CLAUDE_CONFIG_DIR`. Without `CLAUDE_CONFIG_DIR` the item is
plain `Claude Code-credentials`, your default login. herdsman never reads or writes that one, only
the hashed items of the `[accounts.usage]` folders.

herdsman reads the item's account attribute, then its value, with `security find-generic-password`.
When the access token has expired, herdsman refreshes it and writes the rotated login back to the
same item (same service and account) the way Claude Code does: `security -i`, with the value
hex-encoded on stdin. It then re-reads the item to check that the new token was stored. A refresh
rotates the refresh token, so if the write-back fails, the login is gone. herdsman then logs
`usage-login-lost` (sent to notifiers) naming the account and the fix, `claude /login in <dir>`.
An item whose service or account holds a quote or another unusual character is not refreshed at
all. These logins are used only for the usage read and are never exported to lanes.

The meter is read at most every 10 min per account, one read at a time. A 429 backs off for at
least its `retry-after`. The cache, `<state>/usage/<name>.json` (mode 0600), holds numbers and
reset times only. `herdsman accounts` shows (the Host tab shows the same):

```
A        ok                           1 lanes app-impl-12
         session 9% (resets 10m) · week 22% (resets Fri 11:59)
         5h 1.5 · 7d 12.0 herdsman lane-hours
```

The first line is the cap state (`ok`, or `capped 02:10 · resets 07:10`, the logged cap time and its reset) and the lanes on the account now.

Each value has a thin bar on the page: amber at 75 % or more, red at 90 % or more. When the weekly rate
would run out before the reset, the line adds `at this pace: out Mon evening`. If a login is
missing or expired, the line reads `usage: login needed (claude /login in <dir>)`, and the page
still loads.

At spawn, when every uncapped account of the role has a reading under 30 min old, the one with the
lowest weekly % goes first. Otherwise the configured order holds.

**Lane-hours** are what herdsman can measure itself. For each account, they are the hours its lanes
ran in the last 5 h and 7 d, taken from the event log: from the spawn to the issue's merge or clean,
the lane's `lane-dead`, a supersede, or now. They are not Anthropic's quota.

## DeepSeek (dsh lanes, Hermes lanes using DeepSeek)

The key lives in an env file, like the Claude tokens:

```toml
[providers.deepseek]
key_env = "DEEPSEEK_API_KEY"     # the variable name
# env_file = "~/.config/deepseek.env"   # default: [accounts].accounts_env, else herdsman's environment
# warn_below = 5                 # amber, and `provider-low` from `herdsman check`, under this balance
```

herdsman reads `GET https://api.deepseek.com/user/balance` with `curl`, at most every 10 min.
The key goes in a curl config on stdin, never in argv, so `ps` can't show it. The Host tab and
`herdsman accounts` show `deepseek · USD 2.28 · ok`. The row is hidden when no key is set. If `env_file` (or `accounts_env`) is set but unreadable, or
lacks the variable, a dim `deepseek · key not found in <file>` row shows instead.
`herdsman check` logs `provider-low` once each time the balance drops below `warn_below`.

## Hermes lanes

Hermes profiles own their provider keys, in `~/.hermes/profiles/<profile>/.env`. herdsman never
reads them.
