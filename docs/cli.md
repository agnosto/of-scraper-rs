# CLI reference

After `cargo build --release` the binary is `target/release/of-scraper-rs`.
Running it with no arguments opens the interactive TUI (pick a creator →
pick content → scrape). Every command also exists as a plain CLI flag form
below; both write to the same config, download tree and databases.

Files are resolved the same way for every command: `config.conf`,
`auth.json`, `device.wvd`, `state.db` and `of-scraper-rs.log` are looked
for (in order) next to the binary, in the current working directory, then
in the OS config directory (`~/.config/of-scraper-rs` on Linux). The
first directory that contains a given file wins, and files are created
there when missing — so a directory holding `config.conf` is your "config
directory".

## Commands

| Command | What it does |
|---|---|
| [`scrape <user>`](#scrape) | Download a creator's (or all purchased) content |
| [`list`](#list) | List your active subscriptions |
| [`like`, `--unlike`](#like--unlike) | Like or unlike all of a creator's content |
| [`link <url>`](#link-url) | Download one post or message by link |
| [`db …`](#db--download-database) | Manage the cross-run download database (no auth needed) |
| [`check-update`](#check-update) | Check GitHub for a newer release (no auth needed) |

## scrape

```
of-scraper-rs scrape <user> [-c/--content-type <type>] [--force] [--metadata-only]
```

`<user>` is the username (or user id). The special value `ALL_PURCHASES`
walks every creator that appears in your purchase history and scrapes
their purchased content only (works even for accounts you no longer
subscribe to, and for creators whose account was deleted).

`--content-type` accepts:

| Value | Downloads |
|---|---|
| `all` *(default)* | posts, chats (messages), stories |
| `posts` | timeline posts, including the built-in *Archive* label content |
| `chats` | messages (yours can be filtered off with `IgnoreOwnMessages`) |
| `stories` | stories |
| `highlights` | highlight groups (and their stories) |
| `labels` | every label on the account, one folder per label |
| `purchases` | PPV content you bought from this creator |

Flags:

- `--force` — re-download media even when `state.db` already records it
  (bypasses `Download.SkipExistingMedia` for this single run; the log
  prints `(forced)` next to skipped-anyway entries).
- `--metadata-only` — walk the API and write the per-creator
  `Metadata/user_data.db` without downloading anything. Implies metadata
  capture even if `Metadata.SaveUserDb` is `false` in `config.conf`.
  Useful for backfilling the archive database for creators whose files
  you already have.

Example — everything from one creator, forcing re-download:

```
of-scraper-rs scrape peachnecctar --content-type all --force
```

Progress runs through the same TUI-style report the interactive flow
shows, and finishes with a one-line summary when the terminal isn't a
TTY.

## list

```
of-scraper-rs list
```

Prints your active subscriptions one per line. Add expired/restricted
subscriptions to the list via the `Subscriptions` config keys
(see `docs/config.md`).

## like / unlike

```
of-scraper-rs like <user> [-c/--content-type <type>] [--unlike]
```

Likes (or with `--unlike` unlikes) **all** content in the chosen type:
`posts` *(default)*, `chats`, `stories`, or `all` for the three at once.
It prints a running tally of `(changed/skipped/failed)` items. Fails are
expected for items you can't interact with (e.g. locked PPV, archived).

## link <url>

```
of-scraper-rs link <url> [--force] [--metadata-only]
```

Downloads a single item instead of a whole creator. Two link shapes are
understood:

- post — `https://onlyfans.com/{postId}/{username}`
- message — `https://onlyfans.com/my/chats/chat/{userId}/?firstId={messageId}`
  (the URL from a chat message's share/copy action)

Same flags as `scrape`: `--force` bypasses the download database for this
run, `--metadata-only` records the item in `user_data.db` without
downloading. The item is placed in the creator's normal
`Messages`/`Posts` tree at your `DownloadPath`.

## db — download database

`state.db` (a SQLite database next to your config files) remembers every
successfully downloaded media id so later runs skip them
(`Download.SkipExistingMedia`, on by default). This command group manages
it directly and **needs no OnlyFans authentication** — it works offline.

```
of-scraper-rs db path                          # print where state.db resolves to
of-scraper-rs db stats [--user <creator>]      # totals for the db or one creator
of-scraper-rs db list [--user <u>] [--limit N] [--offset M]   # newest first
of-scraper-rs db clear [--user <u>] [--yes]    # delete records (prompt unless --yes)
of-scraper-rs db backfill [--user <u>] [--download-path <root>]
```

- `db clear --user <creator> --yes` — remove one creator's records.
  Omitting `--user` wipes **every** recorded download (the next scrape
  re-considers everything that's still on disk only via
  If-Modified-Since). `--yes` skips the confirmation prompt.
- `db backfill` — scan your existing download tree and record media ids
  found in filenames, so already-downloaded content is skipped in future
  runs. **It can only see ids that literally appear in filenames**: your
  `File.*FileNameFormat` templates must contain `{mediaId}` (a
  `6+`-digit run inside the rendered name is matched). Constraints:
  - Files named by the *default* CDN scheme (`1536x2048_hash.jpg`,
    `xxx_source.mp4`, plain `<mediaId>.mp4` DRM files …) can't be matched
    this way when your template also mixes other placeholders around
    `{mediaId}`; the tool reports how many files were unmatchable and
    where their folders are.
  - Two placeholders touching (`{id}{mediaId}`) are rejected — digits
    would run together.
  - Unmatchable files are still covered by the downloader's built-in
    `If-Modified-Since` check: re-scraping costs only API listing time.
- `db list` shows `downloaded_at` and full row details newest first —
  handy to confirm whether a given item will be skipped or is being
  re-downloaded.

Typical recovery flow after clearing/moving your download folder:

```
of-scraper-rs db backfill                 # import what's on disk
of-scraper-rs db stats                    # sanity check totals
of-scraper-rs scrape --metadata-only ...  # restore per-creator metadata dbs
```

## check-update

```
of-scraper-rs check-update
```

Asks GitHub Releases for the latest version and prints it next to the
running version, plus the release page link. Needs no OnlyFans auth.
It only *checks* — it never downloads or replaces anything; you update
by grabbing the platform zip from the releases page or rebuilding.

Behavior notes:

- Works even when `Updates.CheckForUpdates` is off in `config.conf`
  (that setting only controls the automatic TUI check).
- Always bypasses the 24h cache and refreshes it.
- Other commands print a one-line update notice to **stderr** if a fresh
  cached result says a newer release exists (stdout stays clean for
  scripts).

In the TUI (when the automatic check is enabled in `config.conf`), the
main menu shows an update line once a newer release is known; press
`[U]` to open the release page in your browser.

## DRM (Widevine) downloads

DRM-protected videos decrypt with `ffmpeg` plus a Widevine device file:

1. ffmpeg must be on your `PATH`, or `External.FFmpegPath` set
   (see `docs/config.md`).
2. A `device.wvd` Widevine device file in your config directory. If
   missing, DRM media are logged as skipped (`no DRM device`).

## TUI keys and flows

Interactive behavior worth knowing:

- **Creator picker**: if loading subscriptions fails, the picker shows the
  error with `[R] retry` (the retry itself already waits out 429 rate
  limits and paces the listing pages) and `[Esc] back`. Entering the
  screen again also starts a fresh load — stale errors never stick, and a
  late success flips the error screen straight into the picker.
- **Scraping screen**: while a scrape runs, `[Q]`/`[Esc]` asks first
  (`[Y] abort / [N] continue`). Confirmed abort: any queued-but-unstarted
  downloads are dropped, the ≤4 already-running ones are allowed to
  finish (the footer shows how many are draining), and once drained you
  land back on the content-type checklist. Nothing keeps running invisibly.
- **Like/unlike screen**: `[Q]`/`[Esc]` asks first (`[Y] stop / [N]
  continue`); the run stops between items and returns one screen back.
- **After a completed run**: the "what next" screen offers scraping
  another creator, liking another, the main menu, or quitting.

## Errors and logging

Failures print to the terminal and, per `LoggingLevel`, to
`of-scraper-rs.log` next to `config.conf` (default level logs errors
only). Network/cache errors on a single item never abort a run — the
item is reported and the scrape continues.
