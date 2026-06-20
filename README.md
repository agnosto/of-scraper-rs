# of-scraper-rs

A Rust CLI/TUI scraper for OnlyFans content, built on the request-signing
groundwork from [GentleMercenary/Onlyfans-notifications](https://github.com/GentleMercenary/Onlyfans-notifications),
with a config/auth file layout modeled loosely after the old (now-defunct)
OFDL tool. The OFDL-style `config.conf` format is kept for familiarity and
future compatibility, but **most of its keys are not implemented yet** —
see below for exactly what's real right now.

## Status: what actually works

- **Auth**: `auth.json` (`cookie`, `x_bc`, `user_agent`) is the primary,
  preferred auth method. It's resolved from, in order: next to the
  binary, the current working directory, then the OS config dir
  (`~/.config/of-scraper-rs` on Linux, `~/Library/Application Support/of-scraper-rs`
  on macOS, `%APPDATA%\of-scraper-rs` on Windows).
  - If `auth.json` is missing, there's a fallback that reads
    `Sess`/`Auth_Id`/`User_Agent`/`X_Bc` directly from `config.conf`'s
    `Auth{}` block (see `config.conf-example`). The underscores in those
    key names matter — the parser lowercases keys but doesn't otherwise
    normalize them (`User_Agent` → `user_agent`, not `UserAgent` →
    `useragent`), so renaming them will silently break the fallback.
    `DisableBrowserAuth` in that same block is unused.
- **`config.conf`**: only `Download.DownloadPath` and the `Auth{}`
  fallback fields above are actually read. Everything else in this file —
  `External{}`, `Download.Media{}` (all the `Download*` booleans),
  `IgnoreOwnMessages`, `BypassContentForCreatorsWhoNoLongerExist`,
  `DownloadDuplicatedMedia`, `SkipAds`, `DownloadOnlySpecificDates`,
  `CustomDate`, `DownloadVideoResolution`, `File{}`, `CreatorConfigs{}`,
  `Folder{}`, `Subscriptions{}`, `Interaction{}`, `Performance{}`,
  `Logging{}` — is parsed without error (so the file won't crash the
  scraper) but **none of it currently changes behavior**. It's there as
  groundwork to wire up later, not a working feature.
- **`device.wvd`**: optional Widevine device file for DRM-protected
  content, resolved the same way as the auth/config files. If absent,
  DRM content is simply skipped.
- **Content types**, picked per-scrape (TUI checklist or
  `--content-type` CLI flag), not via config toggles:
  - Posts
  - Chats / Messages
  - Stories
  - Purchased content (PPV posts and messages, by `?author=username` —
    works even for deleted/banned accounts, since that lookup doesn't
    need a resolved numeric user id)
  - Highlights (flattens all stories inside a highlight reel into one
    downloadable bundle)
  - Labels / folders (including the built-in "Archive" pseudo-label),
    downloaded into per-label subfolders
- **TUI flow**: Main Menu → creator picker (type-to-filter against your
  subscriptions) → content-type checklist → live scrape progress screen
  → "what next" screen (scrape another creator / back to menu / quit).
  Also has a Donate screen and shows the repo link + resolved config path
  on the main menu.
- **CLI**: `of-scraper-rs scrape <user> --content-type <type>` and
  `of-scraper-rs list` (lists your subscriptions). No subcommand drops
  you into the TUI.
- DRM (Widevine) downloads via ffmpeg, with progress tracked accurately
  (no race between a just-spawned download task and the "are we done yet"
  check — that bug existed and is fixed).

## In progress / not implemented

- Wiring up the rest of `config.conf`: per-content-type download toggles,
  custom filename formats (`File{}`), per-post/per-message folder
  structuring (`Folder{}`), date-range filtering
  (`DownloadOnlySpecificDates`/`CustomDate`), download rate limiting
  (`Performance{}`), per-creator overrides (`CreatorConfigs{}`),
  subscription filtering (`Subscriptions{}`), non-interactive/batch mode
  (`Interaction{}`), and structured log levels (`Logging{}`).
- `BypassContentForCreatorsWhoNoLongerExist` specifically — purchases
  already work for deleted accounts regardless of this flag; extending
  that same bypass to posts/chats/stories (where it's currently blocked
  on needing a resolved numeric user id) is the next logical step here.
- Avatar/header photo downloads, live stream recording, notification
  scraping (the underlying API client has the types for these — see
  `ContentType::Streams`/`Notifications` — but nothing in `main.rs`
  scrapes them yet).
- Date-based/incremental scraping (`DownloadPostsIncrementally`).
- Ad post filtering (`SkipAds`).
- Per-creator config overrides.

## Disclaimer

For personal archival of content you have legitimate access to. Respect
OnlyFans' Terms of Service and creators' rights.
