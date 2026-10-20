# Configuration reference (`config.conf`)

Everything below reads from `config.conf`, which is looked for (in order)
next to the binary, in the current working directory, or in the OS config
directory (`~/.config/of-scraper-rs` on Linux, `~/Library/Application Support/of-scraper-rs`
on macOS, `%APPDATA%\of-scraper-rs` on Windows). `auth.json`, `device.wvd`,
`state.db` and `of-scraper-rs.log` are resolved the same way — see
`docs/cli.md` for the exact file semantics.

Syntax: `[Section { ... }]`-style blocks, one `Key = value` per line, `#`
marks a comment to end-of-line. Keys are case-insensitive; string values
may be quoted. Any missing key silently keeps its built-in default, so a
minimal config file works.

**Config self-update**: when the scraper starts, it checks your
`config.conf` for keys/sections it recognizes but the file doesn't
mention yet (typically after an upgrade ships new options) and writes
them in — with their default value plus a short comment — without
touching any line you already have. New options therefore show up in an
old file automatically, keeping whatever you had set intact. A file
that doesn't exist yet is left alone (copy `config.conf-example` to
start yours).

Table of contents:

- [Download](#download)
- [Media](#downloadmedia) (hard filters)
- [Deduplication (two layers)](#deduplication)
- [Metadata](#metadata)
- [File — filename formats](#file)
- [Folder](#folder)
- [Subscriptions](#subscriptions)
- [Performance](#performance)
- [External](#external) (ffmpeg / DRM)
- [Logging](#logging)
- [Auth](#auth)
- [Recognized but unimplemented keys](#no-ops)

Every key is also shown with inline comments in `config.conf-example`,
which is kept in sync with the parser by a test.

## Download

All keys live in the `Download { ... }` block.

| Key | Default | Meaning |
|---|---|---|
| `DownloadPath` | _(blank)_ | Where content is saved; blank = `./data` relative to the working directory. |
| `IgnoreOwnMessages` | `false` | Skip chat messages you sent yourself (matched by your `auth_id` from `auth.json`). |
| `DownloadDuplicatedMedia` | `false` | **In-run** dedupe: when `false` (default), the same media id appearing twice inside one run is downloaded only once. See [deduplication](#deduplication) for how this differs from `SkipExistingMedia`. |
| `SkipExistingMedia` | `true` | **Cross-run** dedupe: skip media whose id is already recorded in `state.db` (the download database next to your config files). One `scrape --force` / `link --force` bypasses this for that single run. |
| `SkipAds` | `false` | Skip posts/messages whose text contains `#ad`, `free trial`, or an `onlyfans.com/free` link. |
| `DownloadOnlySpecificDates` | `false` | Enable date filtering for posts (including label posts). Requires `CustomDate`. |
| `DownloadDateSelection` | `"before"` | `"before"` downloads posts older/equal than `CustomDate`, `"after"` newer/equal. |
| `CustomDate` | _(blank)_ | `yyyy-mm-dd` cutoff used by the date filter. |
| `DisableTextSanitization` | `false` | With a custom filename format, `{text}` is normally made filename-safe and truncated to 80 chars; `true` keeps the raw text (may produce long/ugly names). |

## Download.Media (hard filters)

Inside `Download { Media { ... } }`. Every key defaults to `true`, which
means "leave it to the TUI/CLI selection". Setting `false` makes the
filter unconditional — the content is skipped even when selected:

| Key | Meaning |
|---|---|
| `DownloadPaidPosts` / `DownloadPosts` | Filter PPV vs free posts. |
| `DownloadPaidMessages` / `DownloadMessages` | Filter PPV vs free messages. |
| `DownloadArchived` | `false` skips the built-in "Archive" label. |
| `DownloadStories` / `DownloadHighlights` | Skip those content types entirely. |
| `DownloadImages` / `DownloadVideos` / `DownloadAudios` | Filter by media type everywhere, including purchases. GIF videos count as videos. |

`DownloadAvatarHeaderPhoto`, `DownloadStreams` are accepted but are
[no-ops](#no-ops) today.

## Deduplication

Three mechanisms cooperate, and it's worth knowing which does what:

1. **`DownloadDuplicatedMedia` (in-run)** — protects against the same
   media id showing up twice within one scrape (purchases + labels, for
   example). Default: skip duplicates.
2. **`Download.SkipExistingMedia` (cross-run, default on)** — consults
   `state.db` (in the config directory, resolved like `auth.json`; same
   lookup order as above). A successful download (or a
   "file already up to date" HTTP 304 from the CDN) records the media id
   there, and later runs skip it before even queueing. Manage it without
   touching the scraper itself: `db stats`, `db list`, `db clear`,
   `db backfill` — see `docs/cli.md`.
3. **HTTP `If-Modified-Since`** — regardless of any database, when the
   target file already exists on disk with a matching mtime, the CDN is
   asked for changes only and the file is skipped when unmodified. This
   still applies *while* `SkipExistingMedia` is off, and it works on
   files you collected before any database existed (though it means
   re-listing every post through the API).

Practical note: a freshly emptied download folder means no on-disk
evidence; the first post-database run re-queues everything once. Use
`scrape --force` deliberately (it re-downloads *everything* this run).

## Metadata

Inside `Metadata { ... }`:

| Key | Default | Meaning |
|---|---|---|
| `SaveUserDb` | `false` | While scraping, keep a per-creator `Metadata/user_data.db` under that creator's download directory: posts and messages (id, text, price, paid, date), one `medias` row per media (media id, CDN link, local `directory` + `filename`, `downloaded` flag, size, `preview` flag while locked) and a `profiles` row. Also filled by `scrape --metadata-only`, which walks the API and writes the database **without downloading anything** — the way to backfill creators whose files you already have. |

Notes:

- The file intentionally mirrors the SQLite format DIGI's OnlyFans
  scraper produces (`user_data.db`, same tables/columns and the
  `alembic_version` marker), so tools that read that schema read ours.
- The database is opened in WAL mode and tolerates outside readers while
  a scrape runs (any SQLite tool, including DB Browser, can open it
  mid-run; the "please enter encryption key" prompt DB Browser can
  flash during active writes is header misdetection, not encryption).
- The `medias.directory` column uses this scraper's real folder layout
  (relative to `DownloadPath`), so "find this file" lookups resolve to
  actual files.
- Capture happens at scrape time *before* the download-dedupe skips, so
  metadata is recorded even for media that is skipped from downloading.

## File — filename formats

Four independent slots, applied by content bucket:

| Key | Applied to |
|---|---|
| `PostFileNameFormat` | free posts |
| `PaidPostFileNameFormat` | PPV posts (and purchased posts) |
| `MessageFileNameFormat` | free messages |
| `PaidMessageFileNameFormat` | PPV messages (and purchased messages) |

| Placeholder | Renders |
|---|---|
| `{id}` | post/message id |
| `{postedAt}`, `{createdAt}` | post date / message date, `yyyy-mm-dd` |
| `{mediaId}` | the media item's own id |
| `{mediaCreatedAt}` | the media item's own creation date |
| `{filename}` | the original filename as the CDN served it |
| `{username}` | creator handle |
| `{text}` | sanitized post/message text, truncated to 80 characters |
| `{rawText}` | unsanitized, untruncated text (can exceed OS filename limits — prefer `{text}`) |

Case-insensitive; unknown placeholders stay verbatim; the rendered name
is truncated to 80 characters total; if it doesn't contain an extension
but the original had one, the extension is appended.

**Blank template = "whatever OnlyFans calls the file".** What that means
in practice, in the order the code falls back:

1. Plain images/videos: the CDN filename — `0go68tal5..._source.mp4`,
   `2268x4032_d6101fa1...jpg`, and for some uploads a plain hash name
   like `X9D3ws1i0IIh...mp4` (OnlyFans serves those for certain older or
   special uploads; it is the CDN's own naming, not something this
   scraper invents).
2. DRM (Widevine) videos: a locked mp4 has no filename at all — the
   name only exists in the manifest — so the fallback is `<mediaId>.mp4`
   (e.g. `4747346700.mp4`). This is also what `{filename}` renders as for
   a DRM file when you *do* set a custom format.

A format that works for both kinds and is friendly to dedupe/stash-style
matching: `"{postedAt}_{filename}"` (renders e.g.
`2023-05-12_4747346700.mp4` for DRM and `2023-05-12_1536x2048_f720...jpg`
for images).

| Key | Default | Meaning |
|---|---|---|
| `RenameExistingFilesWhenCustomFormatIsSelected` | `false` | `true` = on every scrape, files still sitting under the old/default name in the same folder are renamed to the new format. Non-DRM only (DRM files have no old name to match). |

## Folder

| Key | Default | Meaning |
|---|---|---|
| `FolderPerPost` / `FolderPerPaidPost` | `false` | Put each post's media in its own `yyyy-mm-dd_<postId>` subfolder under the content folder. |
| `FolderPerMessage` / `FolderPerPaidMessage` | `false` | Same for messages. |

Directory layout produced by a scrape (labels/highlights may insert an
extra level):

```
<DownloadPath>/<username>/<Posts|Messages|Stories|Highlights|Labels|Purchases>[/label][/yyyy-mm-dd_<id>]/<Images|Videos|Audios>/<filename>
```

## Subscriptions

| Key | Default | Meaning |
|---|---|---|
| `IncludeExpiredSubscriptions` | `false` | Also list expired subscriptions in the TUI picker and `list`. |
| `IncludeRestrictedSubscriptions` | `false` | Also list restricted ones. |

## Performance

| Key | Default | Meaning |
|---|---|---|
| `Timeout` | `-1` | Per-request HTTP timeout in seconds; `-1` = the client default. |
| `LimitDownloadRate` | `false` | Cap download throughput. |
| `DownloadLimitInMbPerSec` | `4` | The cap, in MB/s, used when `LimitDownloadRate` is `true`. |

## External

| Key | Default | Meaning |
|---|---|---|
| `FFmpegPath` | _(blank)_ | Full path to an ffmpeg binary used for DRM decryption. Blank = whatever `ffmpeg` is on your `PATH`. Required at runtime for DRM content along with a `device.wvd` file (see `docs/cli.md#drm`). |

## Logging

| Key | Default | Meaning |
|---|---|---|
| `LoggingLevel` | `"error"` | File-log verbosity: `off|error|warn|info|debug|trace`. The log lives next to `config.conf` (`of-scraper-rs.log`). |

## Auth

Only consulted when `auth.json` is missing. Fill `Sess`, `Auth_Id`,
`X_Bc` from your browser's `sess=`, `auth_id=` and `X-Bc` cookie/header
values for onlyfans.com, plus `User_Agent`. When `auth.json` exists it
wins. `DisableBrowserAuth` is accepted but has no effect (no
browser-auth flow exists).

## No-ops

These keys are parsed and ignored, kept for compatibility with
configs from sibling tools (everything else above is live):

`DisableBrowserAuth`, `DownloadAvatarHeaderPhoto`, `DownloadStreams`,
`DownloadPostsIncrementally` (the *download* side of "don't repeat" is
the state database — see [deduplication](#deduplication)),
`BypassContentForCreatorsWhoNoLongerExist` (purchases already work for
deleted accounts via the `?author=` lookup), `ShowScrapeSize`,
`DownloadVideoResolution`, every `CreatorConfigs` entry,
`IgnoredUsersListName`, all `Interaction` keys.

## Files the scraper uses

| File | Where | Purpose |
|---|---|---|
| `config.conf` | config dir (binary dir → cwd → OS config dir) | settings (this page) |
| `auth.json` | same | session authentication; auto-created after login in the TUI |
| `device.wvd` | same | Widevine device for DRM decryption (optional) |
| `state.db` | same (SQLite) | cross-run download memory behind `SkipExistingMedia` + `db` commands |
| `of-scraper-rs.log` | same | file log |
| `<DownloadPath>/<user>/Metadata/user_data.db` | per creator | optional per-creator archive db (see [Metadata](#metadata)) |
