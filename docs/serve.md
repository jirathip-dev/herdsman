# The phone page

`herdsman serve --addr <host:port>` is one phone page (Home Screen app, works offline with a
`stale` badge) with Queue · Lanes · Host · Events tabs. `--read-only` serves GET only.

Its only writes are ▲/▼ (`queue mv`), Pause/Resume and a role's model/effort (`set`, choices
from `models = [...]`), each through the CLI's own code path, logged with `"via":"web"`.

A POST must name the bind address (or a `[serve].allowed_hosts` name, e.g. the
`tailscale serve` `*.ts.net` host) in `Host`/`Origin`, and is refused `STALE` when
fleet.toml changed since the page was rendered.
