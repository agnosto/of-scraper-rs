# of-scraper-rs

A Rust CLI/TUI scraper for OnlyFans content, built on the request-signing
groundwork from [GentleMercenary/Onlyfans-notifications](https://github.com/GentleMercenary/Onlyfans-notifications),
with a config/auth file layout modeled loosely after the old (now-defunct)
OFDL tool. The OFDL-style `config.conf` format is kept for familiarity and
future compatibility, but **most of its keys are not implemented yet** —
see [Status](#status-what-actually-works) below for exactly what's real
right now.

## Setup

### Getting a binary

Either grab a build from the GitHub Actions artifacts (this repo has a
`workflow_dispatch` build workflow at `.github/workflows/build.yaml` that
cross-compiles for Linux, Windows, and macOS and bundles
`config.conf-example`/`auth.json-example` alongside the binary — trigger
it manually from the Actions tab, or build from source:

```sh
git clone https://github.com/agnosto/of-scraper-rs.git
cd of-scraper-rs
cargo build --release
```

The binary lands at `target/release/of-scraper-rs` (or `.exe` on
Windows). This needs a reasonably recent stable Rust toolchain — the
crate uses the 2024 edition, which requires **Rust 1.85 or newer**
(`rustup update stable` if `cargo build` complains about the edition).

### Getting your OnlyFans credentials

This scraper authenticates as you, using your existing logged-in session
— there's no username/password login flow, you're handing it your
browser's session cookie.

1. Log into [onlyfans.com](https://onlyfans.com) in a desktop browser.
2. Open DevTools (`F12`, or right-click → Inspect) and go to the
   **Network** tab.
3. Filter requests by `api2` (this hides the noise and shows only the
   signed API calls).
4. Click any `api2` request (e.g. one to `/api2/v2/users/...` — just
   browse around the site a bit if nothing shows up yet) and look at its
   **Request Headers**.
5. From there, copy three things:
   - `Cookie` — the *entire* header value, semicolons and all.
   - `x-bc`
   - `User-Agent`

These expire/rotate periodically (especially the cookie), so if the
scraper starts failing auth, repeat this and refresh whichever file
you're using below.

### Option A: `auth.json` (recommended)

Copy `auth.json-example` to `auth.json` (next to the binary, in the
current directory, or in the OS config dir — see below) and fill it in
directly with what you copied:

```json
{
	"auth": {
		"cookie": "<paste the entire Cookie header value here>",
		"x_bc": "<paste x-bc here>",
		"user_agent": "<paste User-Agent here>"
	}
}
```

### Option B: `config.conf` fallback

Only used if `auth.json` is missing. Copy `config.conf-example` to
`config.conf` and fill in the `Auth{}` block:

```
Auth {
  DisableBrowserAuth = false
  Sess = ""       # the "sess=" value out of the Cookie header, not the whole cookie
  Auth_Id = ""    # the "auth_id=" value out of the Cookie header
  User_Agent = ""
  X_Bc = ""
}
```

The `Cookie` header is a `;`-separated list of `key=value` crumbs — find
the `sess=...` and `auth_id=...` crumbs specifically and use only their
values here, not the full cookie string. The underscores in `Auth_Id`/
`User_Agent`/`X_Bc` matter; see the comment in `config.conf-example` for
why.

### Where these files are looked for

For `auth.json`, `config.conf`, and `device.wvd` alike, in this order:

1. Next to the binary itself.
2. The current working directory (so running via `cargo run` from the
   repo just works).
3. The OS-standard config directory:
   `~/.config/of-scraper-rs` (Linux), `~/Library/Application Support/of-scraper-rs`
   (macOS), `%APPDATA%\of-scraper-rs` (Windows).

### Setting a download path

In `config.conf`, set `Download.DownloadPath` to wherever you want
content saved. Defaults to a `data` folder relative to wherever the
scraper is run from if left blank.

### Optional: DRM content

Some paid videos are Widevine-DRM-protected. If you have a `device.wvd`
file, drop it in alongside your `auth.json`/`config.conf` (same lookup
order as above) and DRM content will be decrypted automatically via
`ffmpeg`. Without one, DRM-protected media is just skipped.

If you have separate client id and private key files instead of a
`.wvd`, you can convert them using
[this tool](https://emarsden.github.io/pssh-box-wasm/convert/).

Either way, `ffmpeg` itself needs to be installed and on your `PATH`,
since the scraper shells out to it for any DRM downloads:

- **Windows**: see [this guide](https://phoenixnap.com/kb/ffmpeg-windows)
  for installing ffmpeg and adding it to your `PATH`.
- **macOS**: `brew install ffmpeg`.
- **Linux**: install `ffmpeg` via your distro's package manager (e.g.
  `apt install ffmpeg`, `pacman -S ffmpeg`).

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
