# tedbot

Discord bot in Rust (poise + serenity). Being extended to integrate with Ted's
home media server.

## Current state of the code

- Workspace with one crate, `tedbot/`. Commands live in `tedbot/src/commands/`
  and are registered in `main.rs` (`FrameworkOptions.commands`).
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

All services bind to 127.0.0.1 only. The bot runs on the same machine.

| Service     | Local URL             | Notes                                                                  |
| ----------- | --------------------- | ---------------------------------------------------------------------- |
| Jellyfin    | http://127.0.0.1:8096 | Public at https://stream.ted.ooo via Caddy                             |
| Seerr       | http://127.0.0.1:5055 | Requests; public at https://request.ted.ooo                            |
| Sonarr      | http://127.0.0.1:8989 | TV                                                                     |
| Radarr      | http://127.0.0.1:7878 | Movies                                                                 |
| Prowlarr    | http://127.0.0.1:9696 | Indexers                                                               |
| qBittorrent | http://127.0.0.1:8080 | Web API; bound to a Mullvad WireGuard tunnel only                      |
| Prometheus  | http://127.0.0.1:9090 | node-exporter, jellyfin, jelly-metrics, macos-process-exporter targets |
| Grafana     | http://127.0.0.1:3000 | Prometheus datasource uid `prometheus`; not public                     |
| ErsatzTV    | http://127.0.0.1:8409 | Live TV channels; XMLTV guide at `/iptv/xmltv.xml`                     |

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
- **Requests channel**: Seerr requests posted to one channel, with approve and
  decline buttons visible only to an admin role. Seerr's built-in Discord
  webhook can post requests but can't act on them, so only the bot can do this.
- **Available channel**: "download complete / available in Jellyfin" posts in
  one channel, pinging the requester if they're linked to the bot. Map Seerr
  users to Discord via the `discordIds` in each Seerr user's notification
  settings instead of a separate link table.
- **`/request <title>`**: search Seerr and return results with poster, year and
  request buttons. For a show, add a dropdown to pick seasons. Lets people
  request without opening request.ted.ooo.
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
- **`/channel`** (done): one ErsatzTV channel per card with its poster. Keypad
  of channel buttons up to 12 channels, ⌨️ jump-to-channel modal beyond that, ◀
  🔄 ▶ always. **`/channels`**: channels in a single vertical list, 12 per page.
  `/channel` takes an autocompleted `number` option. The guide is cached for 60s
  and indexed by channel so this scales to ~1000 channels. Button presses are
  acknowledged before rebuilding (a guide refetch can exceed Discord's 3s
  limit), and failures show as ephemeral follow-ups. Replies are ephemeral;
  ephemeral messages can only be edited with an interaction token, which expires
  after 15 minutes, so buttons time out after 14 and are removed with the newest
  token.
- **`/upcoming`**: new episodes this week for requested shows, from Sonarr's
  calendar. Could also ping the requester on air day.
- **Bot metrics**: expose a Prometheus `/metrics` endpoint from the bot (bound
  to 127.0.0.1, port set in config) with things like commands run per command,
  command errors, latency, gateway status/heartbeat latency, and upstream
  request failures per service. In dotfiles, add a scrape job in
  `prometheus.nix` next to the existing ones and a Grafana dashboard for it
  (Grafana currently only provisions the datasource, so dashboard provisioning
  needs adding in `grafana.nix`).
- Maybe later: admin alerts (tunnel down, disk space, target down, stuck
  downloads), "report a problem" button creating a Seerr issue.

Seerr and Jellyfin (Webhook plugin) can push events by HTTP POST. If the bot
listens for webhooks, bind that listener to 127.0.0.1.

Anything that controls qBittorrent, approves requests, or deletes media must be
restricted to admins: `discord.admin_users` or members with `discord.admin_role`
(`commands::is_admin`). Discord's Administrator permission doesn't count; the
server's admins aren't necessarily the media server's.

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
