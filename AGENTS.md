# tedbot

Discord bot in Rust (poise + serenity). Being extended to integrate with Ted's
home media server.

## Current state of the code

- Workspace with one crate, `tedbot/`. Commands live in `tedbot/src/commands/`
  and are registered in `main.rs` (`FrameworkOptions.commands`). Upstream API
  clients are one module each (`seerr.rs`, `jellyfin.rs`, `arr.rs`,
  `ersatztv.rs`). `webhook.rs` listens for Seerr's webhook, started in `setup`
  once the gateway is ready, and hands new and available requests to
  `announce.rs`.
- Config is read from `$TEDBOT_CONFIG`, falling back to `config.toml` in the
  working directory (gitignored; see `config.example.toml`). Secrets go there,
  never in the repo.
- Edition 2024, stable toolchain via `rust-toolchain.toml`, poise 0.7 / serenity
  0.12.
- `flake.nix` exposes `packages.default` (crane with the rust-overlay
  toolchain), a devShell, and treefmt as `nix fmt`. `nix flake check` runs the
  build, clippy (`-D warnings`), tests and formatting, sharing one dependency
  build.

## Media server it talks to

Runs on `teds-mac-mini` (Apple silicon, macOS 27, `aarch64-darwin`). Everything
is declared in the dotfiles repo at
`/Users/ted/git/dotfiles/hosts/teds-mac-mini/modules/`, one nix module per
service. Read those for the source of truth; the table below is a summary.

All services bind to localhost only. The bot runs on the same machine.

| Service     | Local URL             | Notes                                                                  |
| ----------- | --------------------- | ---------------------------------------------------------------------- |
| Jellyfin    | http://localhost:8096 | Public at https://stream.ted.ooo via Caddy                             |
| Seerr       | http://localhost:5055 | Requests; public at https://request.ted.ooo                            |
| Sonarr      | http://localhost:8989 | TV                                                                     |
| Radarr      | http://localhost:7878 | Movies                                                                 |
| Prowlarr    | http://localhost:9696 | Indexers                                                               |
| qBittorrent | http://localhost:8080 | Web API; bound to a Mullvad WireGuard tunnel only                      |
| Prometheus  | http://localhost:9090 | node-exporter, jellyfin, jelly-metrics, macos-process-exporter targets |
| Grafana     | http://localhost:3000 | Prometheus datasource uid `prometheus`; not public                     |
| ErsatzTV    | http://localhost:8409 | Live TV channels; XMLTV guide at `/iptv/xmltv.xml`                     |

Where credentials live (read at runtime, never commit or copy into the nix
store):

- Sonarr / Radarr / Prowlarr: `<ApiKey>` in `~/.local/share/<app>/config.xml`
- Seerr: `~/.local/share/seerr/settings.json`
- Jellyfin: create a dedicated API key in the dashboard for the bot
- Discord token: the bot's own config file

## Planned features

- **Status command**: Grafana/Prometheus status, either a link to the dashboard
  view or a generated graphic. Grafana's image renderer plugin is awkward on
  macOS; prefer querying Prometheus directly and drawing the chart.
- **Requests channel** (done): posts in each of
  `seerr.webhook.requests_channels` when Seerr's webhook sends `MEDIA_PENDING`
  or `MEDIA_AUTO_APPROVED` (dotfiles' `seerr.nix` turns that type on). The post
  is `/requests`' card as an admin sees it (`requests::post_card`), without the
  list buttons but with 🔄 and a Seerr link button instead of a linked title:
  Approve, Approve all N (its confirmation is ephemeral), Decline, season and
  quality menus, or Cancel request. Anyone sees the buttons, so presses are
  checked with `is_admin`'s role check (`has_admin_role`), except 🔄. The
  buttons have to outlive any command, so presses come through poise's
  `event_handler` (`requests::post_press`), with custom ids
  `request-post:<id>-<action>`. `discord.admin_role`, if set, is pinged for
  requests waiting for approval (the only allowed mention; the role must be
  mentionable or the bot allowed to mention all roles). The webhook keeps each
  open request's posts, found in the channels' last 1000 messages at startup,
  and rebuilds them on `MEDIA_APPROVED`, `MEDIA_DECLINED`, `MEDIA_AVAILABLE` and
  `MEDIA_FAILED`, and every 15 minutes, since Seerr says nothing when a show is
  partly downloaded (`MEDIA_AVAILABLE` waits for every requested season) or a
  download starts. Declined, completed and deleted requests are dropped. Posts
  are only edited when what they show changed: Discord allows only a few edits a
  minute to messages over an hour old. `Data` is in an `Arc` so the webhook
  shares it.
- **Available channel** (done): "now available" posts in each of
  `seerr.webhook.available_channels` when Seerr's webhook sends
  `MEDIA_AVAILABLE`: poster, title, overview, seasons and requester, with
  Jellyfin, Moonfin Web (`jellyfin.moonfin`) and Seerr link buttons. Only posted
  if the request's movie or seasons are available, and once per request per run.
- 4K requests (dotfiles' `radarr-4k`/`sonarr-4k`, Seerr's default 4K servers)
  are read from Seerr's `status4k`, `mediaUrl4k` and `jellyfinMediaId4k`, and
  marked "(4K)". Cancelling one takes it out of the 4K Radarr or Sonarr
  (optional `[radarr_4k]`/`[sonarr_4k]` config), and admins pick its quality on
  `/requests` cards from Seerr's default 4K server's profiles. `/request` cards
  show both versions' status; users with Seerr's Request 4K (or its movie/series
  variant) can request 4K whether or not the HD version is there: Request and
  Request 4K buttons side by side (each gone once that version is requested or
  there), and the season menu shared by both.
- Approved but not downloaded reads just "Approved" until Radarr or Sonarr has
  grabbed a release, then "download queued" until its queue has any bytes of it,
  then "download started" (`arr.rs`), checked only for what's approved.
- Seerr counts a season's episodes by TMDB, which can split seasons differently
  from TVDB (Sonarr's), so a finished season can stay "partly downloaded". The
  bot counts it as downloaded when Sonarr has a file for every monitored episode
  (`check_partial`).
- Both channels' posts come from `announce.rs`. The requester shows as a mention
  (no ping: it's in an embed) when their `discordIds` account is in that
  channel's server, else by Seerr name. They need Send Messages and Embed Links
  in the channel.
- **Webhook** (`webhook.rs`, `[seerr.webhook]` config): listens on localhost
  (`Ipv4Addr::LOCALHOST`) at `/seerr`, plus `GET /health` (200) for the
  dotfiles' Prometheus probe and status page; both are served only once Discord
  is connected. Seerr has only one webhook and Moonfin (a Jellyfin plugin, at
  `/Moonfin/Seerr/Webhook` on Jellyfin) used it, so the bot forwards every
  notification to each of `seerr.webhook.forward` (one URL or a list) at once
  with its Content-Type and Authorization, and answers Seerr with the first
  failed response, else the first one. Logs leave out the query, which may hold
  a secret. Payloads are whatever Seerr's template makes (Moonfin's); the bot
  only reads `notification_type` and `request.request_id`, both in Seerr's
  default and Moonfin's. Seerr's webhook URL is set in dotfiles' `seerr.nix`.
- **`/request <title>`** (done): search Seerr, one ephemeral card per result
  with poster, year, overview and status, ◀ ▶ through results, and a Request
  button. Shows get a season dropdown (no pick means all open seasons; Discord
  caps it at 25 options). Autocomplete fills in `movie:<id>` or `tv:<id>`.
  Requests are made with `X-API-User` as the Seerr user whose `discordIds` match
  the invoker, so Seerr's own permissions, quotas and auto-approve apply. Seerr
  rejects search queries with any unencoded reserved character (even `+`), so
  the query is percent-encoded by hand. Seerr's API will make a new request for
  an available movie, so only show Request when the status is missing.
  `seerr.public_url` links the title to the Seerr page. There's no quality menu:
  Seerr picks the profile (which handles anime); admins can change it on
  `/requests` while the request is pending. Users with Manage Requests and a
  quota get a "Request, bypass quota" button (`ignoreQuota`); Manage Users means
  no quota at all. A "Cancel request" button shows while a request is waiting
  for approval or approved but not downloaded yet, not even in part: requesters
  for their own, bot admins (`is_admin`) for anyone's. The bot checks this
  itself and asks Seerr as its admin. Approved requests come out of
  Radarr/Sonarr directly (`arr.rs`, optional `[radarr]`/`[sonarr]` config):
  queue items removed from qBittorrent, then the movie deleted, or for shows
  only the request's seasons unmonitored (the series goes only if nothing's
  left). Radarr/Sonarr are checked for files first, since Seerr can lag behind,
  and nothing is touched if any exist.
- **`/requests`** (done): ephemeral list of requests, newest first, 10 per page
  with ◀ 🔄 ▶ and an optional status filter. Everyone sees their own (via
  `requestedBy`); bot admins see everyone's. A menu opens one as a card (poster,
  status, seasons, when, and for admins who requested and approved it and the
  quality), with ◀ ▶ through the list. Seerr users show as a mention when their
  `discordIds` account is in the server, like `/streams`, else by Seerr name
  (looked up once per user per invocation). The card offers Cancel request on
  the same terms as `/request` (shared code), and for admins on pending requests
  Approve, Decline, and season and quality menus that edit the request straight
  away (`PUT /request/{id}`, which Seerr only allows while pending and which
  resets every field it's not sent, so the rest are sent back as they were).
  Pending cards also get "Approve all N" when the same requester has other
  pending requests each made within an hour of the one before; it asks to
  confirm with the titles listed, then approves them one at a time and lists any
  that failed. Quality is only editable when the request goes to the default
  server, whose profiles are the ones the bot knows. Seerr is asked as its
  admin, so the bot checks `is_admin` itself. Request lists carry no titles, so
  each row's details are fetched.
- **`/streams`** (done): ephemeral, since views differ. Everyone gets the stream
  count, upload in use and who's watching. Admins also get what each stream is
  playing, direct play or transcode, and LAN bitrate. Bitrates count unpaused
  streams. From Jellyfin's sessions API, with a 🔄 button. Users who set a
  Discord id in Seerr and are in the server show as a mention (embeds never
  ping; `GET /api/v1/user` omits settings, so `discordIds` comes from
  `GET /api/v1/user/{id}`). Bitrate falls back to the source's video plus
  playing audio track when `TranscodingInfo` is missing (Jellyfin drops it while
  a transcode is throttled). Clients on private IPs count as LAN; Caddy is a
  known proxy, so `RemoteEndPoint` is the real client.
- **`/tv`** (done): ErsatzTV channels in a single vertical list, 12 per page.
  With the autocompleted `channel` option, one channel per card with its poster
  instead: keypad of channel buttons up to 12 channels, ⌨️ jump-to-channel modal
  beyond that, ◀ 🔄 ▶ always. The guide is cached for 60s and indexed by channel
  so this scales to ~1000 channels. Button presses are acknowledged before
  rebuilding (a guide refetch can exceed Discord's 3s limit), and failures show
  as ephemeral follow-ups. Replies are ephemeral; ephemeral messages can only be
  edited with an interaction token, which expires after 15 minutes, so buttons
  time out after 14 and are removed with the newest token.
- **`/restart <service>`** (done): only Jellyfin so far. Admins only. Jellyfin's
  launchd agent is `jellyfin.launchd_label`, by default `org.nixos.jellyfin`.
  `default_member_permissions` hides it from members without Administrator;
  Discord can't hide a command by role from a bot token, so the admin role is
  given access in the server's Integrations settings, and the bot checks
  `is_admin` regardless. Confirms with how many streams it would stop, runs
  `launchctl kickstart -k gui/<uid>/<label>` (the bot is an agent in the same
  session), then polls Jellyfin's `/health`.
- Every command reply is ephemeral (`ephemeral` on each `#[command]`, so
  `ctx.say`/`ctx.send` default to it), as are follow-ups to button presses.
- **Bot metrics**: expose a Prometheus `/metrics` endpoint from the bot (bound
  to localhost, port set in config) with things like commands run per command,
  command errors, latency, gateway status/heartbeat latency, and upstream
  request failures per service. In dotfiles, add a scrape job in
  `prometheus.nix` next to the existing ones and a Grafana dashboard for it
  (Grafana currently only provisions the datasource, so dashboard provisioning
  needs adding in `grafana.nix`).
- Maybe later: admin alerts (tunnel down, disk space, target down, stuck
  downloads), "report a problem" button creating a Seerr issue.

Seerr and Jellyfin (Webhook plugin) can push events by HTTP POST. Seerr's
already comes to the bot (`webhook.rs`); new Seerr events go there. Any other
listener binds to localhost too.

Every command (and autocomplete) is only for users whose Discord id is in their
Seerr notification settings: `command_check` in `main.rs` looks them up
(`Seerr::user_for`, refetched at most once a minute), tells anyone else how to
link, and hands commands the Seerr user (`commands::seerr_user`). Request posts'
buttons aren't commands and aren't checked this way.

Anything that controls qBittorrent, approves requests, or deletes media must be
restricted to admins: members with `discord.admin_role` (`commands::is_admin`;
no role configured means no admins, and nobody is an admin in DMs). Discord's
Administrator permission doesn't count; the server's admins aren't necessarily
the media server's.

## Deployment

The bot repo stays free of machine-specific values; dotfiles holds them.

- This repo should expose a `flake.nix` with `packages.aarch64-darwin.default`
  building the bot.
- Dotfiles adds it as a flake input and runs it from a launchd user agent in
  `hosts/teds-mac-mini/modules/tedbot.nix` (`RunAtLoad`, `KeepAlive`, log at
  `~/Library/Logs/tedbot.log`), matching the other modules there.
- Config (token, channel IDs, service URLs) comes from a file or environment
  variables outside the repo and the nix store. The current "read `config.toml`
  from cwd" works if the agent sets `WorkingDirectory`, but an explicit path or
  env var override is cleaner.
- Ship flow: push here, `nix flake update tedbot` in dotfiles, `just switch`
  (needs sudo).
