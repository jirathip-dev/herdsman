# Troubleshooting: macOS Keychain and remote hosts

Pitfalls that each look like a different bug. One section each: symptom, cause, fix, confirm. Never paste a password
or token on a command line: read it with `read -rs` into a shell variable and `unset` it afterwards.

## 1. A locked or jammed login Keychain (#707)

**Symptom.** Lanes on a folder account come up signed out, or `security` calls hang over ssh; the board still serves
and drive still starts lanes (drover itself reads no Keychain item and holds no secret store since #707).

**Cause.** Claude Code keeps a folder login (`CLAUDE_CONFIG_DIR=<folder> claude /login`) in the login Keychain. A locked
login Keychain, or unanswered Keychain dialogs queued in SecurityAgent with no one at the screen (seen on 2026-11-08),
leaves that login unreadable.

**See it.**

```sh
ps -axo pid,etime,command | grep -E '[S]ecurityAgent|[s]ecurity (find|unlock)'
```

A SecurityAgent with a long elapsed time plus `security` processes that never finish is the jam.
`drover check` alerts `keychain-jammed` (once per episode) with their pids and this fix when more than
`[limits] keychain_readers` (3) wait over 60 s; `drover doctor` shows the count. `reap_keychain_readers = true`
lets `check` run the fix itself (`keychain-reap`).

**Fix.** Over ssh, name the login keychain explicitly (an ssh session has no login search list), unlock it, and kill a
jam's waiting `security` calls by pid, then SecurityAgent (macOS restarts it; the queued dialogs count as Deny):

```sh
K=~/Library/Keychains/login.keychain-db
read -rs P   # the login keychain's password, no echo
security unlock-keychain -p "$P" "$K"
unset P
ps -axo pid,command | grep '[s]ecurity find-generic-password'   # note the pids
kill <pid> <pid>
kill "$(pgrep -x SecurityAgent)"
```

A headless host that must not depend on the login Keychain at all: give its accounts a hand-placed `token-file:`
([accounts](accounts.md)).

**Confirm.** `drover accounts` shows each account `signed in`, and a new lane on it starts signed in.

## 2. drover's old Keychain items

**Symptom.** `drover/<fleet>/<NAME>` items (old `secret:` values, `BOAT_API_KEY`, account tokens) are still in the
login Keychain after the move to secret files.

**Cause.** drover stopped reading them (#707) and deletes nothing itself.

**Fix.** Put each value you still need in its file (`<config dir>/secrets/<NAME>`, mode 0600; see
[providers](providers.md)), then run the `security delete-generic-password -a drover -s …` lines `drover migrate`
prints, naming the login keychain over ssh as in §1.

**Confirm.** `security dump-keychain ~/Library/Keychains/login.keychain-db | grep '"drover/'` prints nothing.

## 3. Stale `~/.local/bin/drover` on a remote host

**Symptom.** A remote host was updated (`host-update` logged, `drover --version` there is current), yet its lanes'
hook behaves like an old build.

**Cause.** The updater installs into `~/.local/share/drover/bin`, but the lanes' hook called a separate, older copy
in `~/.local/bin` that is not a symlink, so the update never reached it. #573 is the real fix.

**Check.**

```sh
ssh <host> 'ls -l ~/.local/bin/drover; ~/.local/bin/drover --version'
```

A regular file (no `->`) or a version different from the main host's is the stale copy.

**Fix.** Replace it with a symlink to the updated binary:

```sh
ssh <host> 'ln -sf ~/.local/share/drover/bin/drover ~/.local/bin/drover'
```

**Confirm.** The check above shows `~/.local/bin/drover -> …/.local/share/drover/bin/drover` and the main
host's version and git sha.

## 4. Remote fleet.toml drift

**Symptom.** Every tool call of a lane on a remote host is refused `REFUSED HOOK_FAILED_CLOSED: <fleet> not in
fleet.toml`.

**Cause.** A remote host's own `~/.config/drover/fleet.toml` is written once at cloud bootstrap (only when it has
none) and never refreshed. A fleet added on the main host later is missing there, so the host's lane hook fails
closed.

**Workaround until #592 lands.** Copy the main host's `[fleet.*]` tables (without notifiers) into the remote file, or
move it aside and bootstrap again so it is rewritten:

```sh
ssh <host> 'mv ~/.config/drover/fleet.toml ~/.config/drover/fleet.toml.old'
drover hosts bootstrap <host>
```

**After #592.** drover keeps the remote fleet.toml in step with the main host's, so a new fleet reaches every host
without this step.

**Confirm.** `ssh <host> 'grep -n "^\[fleet.<fleet>\]" ~/.config/drover/fleet.toml'` prints the table, and the lane's
next tool call is not refused.

## 5. Shed on remote hosts

**Symptom.** A remote host shows offline and takes no lanes, though ssh works there.

**Cause.** A host is up only when its shed answers (`shed connect` over ssh); a host with no shed, or its shed
server down, counts as offline.

**How to tell.**

```sh
drover hosts                       # the host: offline
ssh <host> shed ls                 # fails
```

**Fix.** `drover hosts bootstrap <host>` (installs shed and its job), or start shed's job there.

**Confirm.** `drover hosts` shows the host reachable, and a lane placed there starts.

## 6. Wedged Keychain daemon (securityd)

Seen on 2026-11-08 on a headless Mac. Commands marked *(unverified)* come from that outage and were not re-run for
this page; the rest were run once on macOS 27.2 with FileVault and SIP on.

**Symptom.** Every `security` call hangs, not only item reads: `security list-keychains`, `unlock-keychain` and
`show-keychain-info` too. securityd's log loops on an unlock that waits for
a dialog nobody sees:

```sh
perl -e 'alarm 30; exec @ARGV' log show --last 10m --predicate 'process == "securityd"' --style compact | tail -20
```

It repeats `unlocking for makeUnlocked()` followed by `new SecurityAgentXPCQuery`.

**Cause.** securityd itself is stuck behind an unlock prompt, and every new reader (the tick and fast jobs, other
pollers) queues behind it.

**Fix.**

1. Stop what keeps starting readers: drover's timer jobs and any other poller (unload their LaunchAgents, or stop
   them as you started them). *(unverified)*
2. Kill the waiting readers by pid and SecurityAgent, as in §1.
3. Kill the per-session daemon; launchd starts a new one. `launchctl kickstart system/com.apple.securityd` is
   refused while SIP is on, so kill it by pid: *(unverified)*

   ```sh
   ps -axo pid,etime,command | grep '[s]ecurityd -i'   # note the pid
   sudo kill -9 <pid>
   ```

4. Wait a few seconds, then unlock the login keychain (§1):

   ```sh
   K=~/Library/Keychains/login.keychain-db
   read -rs P
   perl -e 'alarm 20; exec @ARGV' security unlock-keychain -p "$P" "$K"
   unset P
   ```

5. Start the jobs from step 1 again.

**Confirm.** A bounded call returns at once and exits 0:

```sh
perl -e 'alarm 10; exec @ARGV' security list-keychains
```

Exit 142 means the alarm fired: still wedged.

### Bounded commands

Wrap every diagnostic `security` or `log` call in `perl -e 'alarm N; exec @ARGV' <command>`: the command is killed
after N seconds (exit 142) instead of hanging the shell. Perl ships with macOS; `timeout` is missing on older macOS
releases (macOS 27.2 has `/usr/bin/timeout`), so the perl form works everywhere.

## 7. Keep a headless Mac's login usable

A macOS host's Claude login lives in the login Keychain (the `Claude Code-credentials` item; see
[accounts](accounts.md)), so a headless Mac on folder logins must keep that keychain unlocked and the Mac awake (or
use a `token-file:` account, #707):

```sh
security show-keychain-info ~/Library/Keychains/login.keychain-db   # want: no-timeout
security set-keychain-settings ~/Library/Keychains/login.keychain-db  # no -t, no -l: never auto-lock (unverified)
pmset -g | grep -E '^ *sleep '                                        # want: sleep 0
sudo pmset -a sleep 0                                                 # (unverified)
```

**FileVault and reboots.** With FileVault on, a plain reboot stops at the pre-boot unlock screen and auto-login is
impossible, so the Mac stays offline until someone types the password at it. For a planned restart use an
authenticated restart, which unlocks the disk once on the way back up: *(unverified)*

```sh
sudo fdesetup authrestart
```

**A shell rc that exports `CLAUDE_CODE_OAUTH_TOKEN`.** A token exported in `~/.zshrc` (or another rc file) reaches
every lane and overrides the account env drover gives it, so all lanes run on that one token. Remove the export;
`doctor` flags a login-shell token (#708). Check without printing the value:

```sh
grep -n CLAUDE_CODE_OAUTH_TOKEN ~/.zshrc ~/.zprofile ~/.bashrc ~/.profile 2>/dev/null | cut -d= -f1
```

## 8. Notes from the outage

- Paste one line at a time. In some terminals a multi-line paste arrives as literal `\n`, and a `read -rs` takes the
  next pasted line as the password. *(unverified)*
- `ls` aliased to `eza` breaks commands copied from docs, e.g. `ls ~/Library/LaunchAgents`; use `/bin/ls` or
  `command ls`.
- Tailscale SSH needs an ACL rule that allows your user on that machine; without it the ssh is refused. *(unverified)*
- While securityd is wedged, Screen Sharing can accept the connection and then fail in the client, so ssh is the way
  in. *(unverified)*
