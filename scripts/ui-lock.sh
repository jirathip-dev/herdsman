#!/usr/bin/env bash
# #560: one UI audit (WebKit or headless Chrome) at a time on this host: run "$@" holding a lock on
# ~/.cache/herdsman/ui-audit.lock (the file names the holder, `$HERDSMAN_LANE`), so lanes queue for it instead of
# rendering side by side. The lock is the kernel's (`flock`, kept across the `exec`): a holder that dies lets go at once;
# one held 10 min is stale and run past. The user's home from the password database, not `$HOME` (a test may point
# that at a temporary dir); `UI_AUDIT_LOCK` names another lock file.
# Usage: scripts/ui-lock.sh <command> [args...]
set -euo pipefail
lock=${UI_AUDIT_LOCK:-$(eval echo "~$(id -un)")/.cache/herdsman/ui-audit.lock}
mkdir -p "${lock%/*}"
exec perl -e '
use Fcntl qw(:flock F_SETFD);
my ($lock, $me) = (shift, shift);
open(my $f, "+>>", $lock) or die "ui-lock: $lock: $!\n";
my $holder = sub { open(my $r, "<", $lock) or return "?"; my $h = <$r> // "?"; chomp $h; $h };
my $said;
until (flock($f, LOCK_EX | LOCK_NB)) {
  if (time - (stat $f)[9] >= 600) {
    print STDERR "taking over the UI audit lock (held by ", $holder->(), " for 10 min)\n";
    last;
  }
  print STDERR "waiting for the UI audit lock (held by ", $holder->(), ")\n" unless $said++;
  sleep 1;
}
truncate($f, 0);
syswrite($f, "$me\n");
fcntl($f, F_SETFD, 0) or die "ui-lock: $!\n";
exec { $ARGV[0] } @ARGV or die "ui-lock: $ARGV[0]: $!\n";
' "$lock" "${HERDSMAN_LANE:-pid $$}" "$@"
