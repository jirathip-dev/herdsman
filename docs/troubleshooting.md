# Troubleshooting: macOS Keychain and remote hosts

Pitfalls that each look like a different bug. One section each: symptom, cause, fix, confirm. Never paste a password
or token on a command line: read it with `read -rs` into a shell variable and `unset` it afterwards.

## 1. Keychain over ssh

**Symptom.** Over ssh, `security find-generic-password -s herdsman/<fleet>/<NAME>` (or `delete-generic-password`)
says "The specified item could not be found in the keychain", and `security unlock-keychain -p "$P"` or `-k "$P"`
rejects the right login password.

**Cause.** An ssh session has no login session's keychain search list, so macOS does not search the login keychain by
default. The item is looked up, and the password tried, against another keychain.

**Fix.** Name the login keychain explicitly on every `security` call:

```sh
K=~/Library/Keychains/login.keychain-db
read -rs P   # the login keychain's password, no echo
security unlock-keychain -p "$P" "$K"
security find-generic-password -s herdsman/<fleet>/<NAME> "$K"
unset P
```

**Confirm.** `security find-generic-password -s herdsman/<fleet>/<NAME> ~/Library/Keychains/login.keychain-db` prints
the item's attributes (not its secret) and exits 0.

## 2. Jammed SecurityAgent

**Symptom.** Every keychain call hangs, unlock included: spawns hold with `REFUSED KEYCHAIN_LOCKED`, `herdsman check`
alerts `keychain-locked`, and approving the dialog is impossible because no dialog is visible (seen after 27 h
uptime).

**Cause.** Unanswered Keychain dialogs, invisible with no one at the screen, queue up in SecurityAgent. Every later
keychain call waits behind them.

**See it.**

```sh
ps -axo pid,etime,command | grep -E '[S]ecurityAgent|[s]ecurity (find|unlock)'
```

A SecurityAgent with a long elapsed time plus `security find-generic-password` processes that never finish is the jam.

**Fix.** Kill the waiting `find-generic-password` calls by pid, then SecurityAgent; macOS restarts it. The queued
dialogs count as Deny.

```sh
ps -axo pid,command | grep '[s]ecurity find-generic-password'   # note the pids
kill <pid> <pid>
kill "$(pgrep -x SecurityAgent)"
```

**Confirm.** The `ps` above lists no waiting `security` calls, and
`security find-generic-password -s herdsman/<fleet>/<NAME> ~/Library/Keychains/login.keychain-db` returns at once.
`herdsman doctor` shows the account rows ✓.

## 3. New build, new cdhash (#548, #631)

**Symptom.** After installing a new herdsman build, its keychain reads time out (`REFUSED KEYCHAIN_LOCKED`) or a
"herdsman wants to use your confidential information" dialog appears, though the item worked with the old build.

**Cause.** Each build has a new cdhash (code directory hash). A token item's partition list names which code may read
it without a prompt. Since #631 herdsman reads its items only through `herdsman-keys`, a small helper installed beside
it (`~/.local/share/herdsman/bin/herdsman-keys`) and reinstalled only when its own source (`crates/keys/`) changes, so
its cdhash, and your approval, last across herdsman builds. `herdsman self-update` and `scripts/install-gate.sh` swap a
build in only when its `doctor --tokens` reads every account token through the helper with no dialog; else they keep
the old build and alert `self-update-failed` with this fix. See [accounts](accounts.md).

**Fix.** Once per host (and again only when the helper's source changes; the alert then names
`herdsman-keys.new`), over ssh, authorise the helper on each item. The login keychain's password can drift from the
Mac's login password (a password reset changes one, not the other); use the keychain's.

```sh
K=~/.local/share/herdsman/bin/herdsman-keys   # or the herdsman-keys.new the alert names
codesign -dvvv "$K" 2>&1 | grep CDHash   # CDHash=<cdhash>
read -rs P   # the login keychain's password, no echo
security set-generic-password-partition-list -S "apple-tool:,apple:,unsigned:,cdhash:<cdhash>" -k "$P" \
  -s herdsman/<fleet>/<NAME> ~/Library/Keychains/login.keychain-db
unset P
```

Repeat the `set-generic-password-partition-list` line for every `herdsman/<fleet>/<NAME>` item. If an item still
prompts, run `"$K" get herdsman/<fleet>/<NAME> >/dev/null` in a terminal at the Mac and click Always Allow (run by
herdsman the helper never shows a dialog; it fails at once instead). Then rerun `herdsman self-update`.

**Confirm.** `herdsman doctor --tokens` and `herdsman doctor` show each account row ✓ with no dialog on the host's
screen.

## 4. Stale `~/.local/bin/herdsman` on a remote host

**Symptom.** A remote host was updated (`host-update` logged, `herdsman --version` there is current), yet its lanes'
hook behaves like an old build.

**Cause.** The updater installs into `~/.local/share/herdsman/bin`, but the lanes' hook called a separate, older copy
in `~/.local/bin` that is not a symlink, so the update never reached it. #573 is the real fix.

**Check.**

```sh
ssh <host> 'ls -l ~/.local/bin/herdsman; ~/.local/bin/herdsman --version'
```

A regular file (no `->`) or a version different from the main host's is the stale copy.

**Fix.** Replace it with a symlink to the updated binary:

```sh
ssh <host> 'ln -sf ~/.local/share/herdsman/bin/herdsman ~/.local/bin/herdsman'
```

**Confirm.** The check above shows `~/.local/bin/herdsman -> …/.local/share/herdsman/bin/herdsman` and the main
host's version and git sha.

## 5. Remote fleet.toml drift

**Symptom.** Every tool call of a lane on a remote host is refused `REFUSED HOOK_FAILED_CLOSED: <fleet> not in
fleet.toml`.

**Cause.** A remote host's own `~/.config/herdsman/fleet.toml` is written once at cloud bootstrap (only when it has
none) and never refreshed. A fleet added on the main host later is missing there, so the host's lane hook fails
closed.

**Workaround until #592 lands.** Copy the main host's `[fleet.*]` tables (without notifiers) into the remote file, or
move it aside and bootstrap again so it is rewritten:

```sh
ssh <host> 'mv ~/.config/herdsman/fleet.toml ~/.config/herdsman/fleet.toml.old'
herdsman hosts bootstrap <host>
```

**After #592.** herdsman keeps the remote fleet.toml in step with the main host's, so a new fleet reaches every host
without this step.

**Confirm.** `ssh <host> 'grep -n "^\[fleet.<fleet>\]" ~/.config/herdsman/fleet.toml'` prints the table, and the lane's
next tool call is not refused.

## 6. Shed on remote hosts

**Symptom.** With a shed runtime configured, a remote host shows offline and takes no lanes, though ssh and herdr work
there.

**Cause.** Until #601 lands, a shed runtime makes herdsman look for shed on every host; a remote host with no shed fails
that look and counts as offline.

**How to tell.**

```sh
herdsman hosts                    # the host: offline
ssh <host> herdr agent list       # works
```

Offline in `herdsman hosts` while `herdr agent list` over ssh is OK is this case, not a network fault.

**Fix.** Until #601, run remote lanes with the herdr runtime (`kind = "herdr"`, see [runtimes](runtimes.md)), or
install shed on that host.

**Confirm.** `herdsman hosts` shows the host reachable, and a lane placed there starts.
