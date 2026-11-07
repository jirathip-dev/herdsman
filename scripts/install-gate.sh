#!/bin/sh
# install-gate NEW DEST (#539): put the freshly built herdsman binary NEW at DEST only if NEW's
# `doctor --config-only` loads the live fleet.toml and its `doctor --tokens` reads the tokens (#631); else keep
# DEST, print the fix and exit 1 (the auto-install job's failure goes to #fleet-ops).
set -u
new=${1:?usage: install-gate.sh NEW DEST}
dest=${2:?usage: install-gate.sh NEW DEST}
if ! out=$("$new" doctor --config-only 2>&1); then
  echo "herdsman install-gate: the new binary can't load the live config; kept $dest"
  echo "$out"
  echo "fix: edit the config as the ✗ line says, then rerun the install"
  exit 1
fi
mkdir -p "$(dirname "$dest")" || exit 1
# #631: the Keychain is read only through herdsman-keys beside DEST, copied in only when its source hash (in
# herdsman-keys.src) changed, so its cdhash and the owner's one approval last; first time in place, else as .new.
# The new binary must read every account token through it, with no dialog, or DEST stays.
keys=$(dirname "$dest")/herdsman-keys
staged=$keys
root=$(dirname "$0")/..
hash=$(cat "$root/crates/keys/Cargo.toml" "$root/crates/keys/src/main.rs" | shasum -a 256 | cut -d' ' -f1)
if [ "$(cat "$keys.src" 2>/dev/null)" != "$hash" ]; then
  [ -e "$keys" ] && staged=$keys.new
  rm -f "$staged" && cp "$(dirname "$new")/herdsman-keys" "$staged" || exit 1
  [ "$staged" = "$keys" ] && echo "$hash" >"$keys.src"
fi
if ! out=$(HERDSMAN_KEYS=$staged "$new" doctor --tokens 2>&1); then
  echo "herdsman install-gate: the new binary can't read the tokens through $staged; kept $dest"
  echo "$out"
  echo "fix: authorise it as the ✗ line says (docs/troubleshooting.md §3), then rerun the install"
  exit 1
fi
cp "$new" "$dest.new" || exit 1
# #556: sign with the one local identity, so every build keeps the keychain's "Always Allow"; without it the
# install goes on ad-hoc signed and `herdsman doctor` shows the signature row red with the setup command.
# #568: a codesign that fails or hangs on a keychain dialog (timed out) keeps DEST and exits 1 with the fix.
if command -v codesign >/dev/null; then
  sh "$(dirname "$0")/signing-identity.sh" sign "$dest.new"
  case $? in
  0) ;;
  2) echo "herdsman install-gate: not signed; run scripts/signing-identity.sh once, then reinstall" ;;
  *)
    rm -f "$dest.new"
    echo "herdsman install-gate: signing failed; kept $dest"
    exit 1
    ;;
  esac
fi
mv "$dest.new" "$dest"
[ "$staged" = "$keys" ] || { mv "$staged" "$keys" && echo "$hash" >"$keys.src"; }
