# Browser overlays

Drop tosu v2 API compatible overlays into this folder and the server renders
them in a browser, so you can use them as OBS **Browser** sources.

## How it works

1. Put each overlay in its own subfolder. A subfolder is picked up as soon as it
   contains an `index.html`; the folder name becomes the URL segment.
2. Open the dashboard at <http://127.0.0.1:24050/overlays/>.
3. Copy a card's URL into an OBS Browser source.

No restart is needed: the directory is re-scanned whenever it changes.

```
browser_overlays/
  My Overlay/            <- served at /overlays/My%20Overlay/
    index.html
    index.js
    index.css
    metadata.txt
    deps/                <- vendored libraries, served as-is
    resources/
```

## Which overlays work

Any overlay written against the **tosu v2** API. Drop-in overlays that hardcode
a tosu address work unchanged: the server injects a small compatibility shim into
every HTML page it serves, ahead of the overlay's own scripts, which rewrites
these endpoints onto whichever origin the page was loaded from.

| Overlay does this | Rewritten to |
| --- | --- |
| `ws://127.0.0.1:24050/websocket/v2` | `ws://<page-host>/websocket/v2` |
| `ws://127.0.0.1:24050/ws` | `ws://<page-host>/ws` |
| `ws://<host>:<port>` | `ws://<page-host>/websocket/v2` |
| `http://127.0.0.1:24050/backgroundImage?mapset=1` | `/files/beatmap/background` |
| `http://127.0.0.1:24050/Songs/...` | `/files/beatmap/...` |

Every row keeps the **path** and only re-homes the host and scheme. `/ws` used to
be rewritten onto `/websocket/v2`, which handed every v1 (gosumemory) overlay the v2
payload; rtosu now serves the real v1 payload on `/ws`, so it is an identity mapping.

`WebSocket`, `fetch`, and `XMLHttpRequest` are all covered, so bundled
`ReconnectingWebSocket` copies work too.

**CSS `url()` is not rewritten.** A stylesheet or inline style such as
`background-image: url("http://127.0.0.1:24050/backgroundImage?mapset=1")` is
resolved by the browser directly and never passes through the shim, so it only
reaches this server when it is on port 24050. If an overlay loads images that
way, point them at the shim's origin instead:

```css
/* works on any port */
background-image: url("/files/beatmap/background?mapset=1");
```

**JSON routes are not rewritten either**, which is now the right answer: `/json`
serves the gosumemory-compatible **v1** payload and `/json/v2` the v2 payload,
on the same paths tosu serves them (`router/index.ts:43`,
`router/v2.ts:4-7`). A v1 overlay that fetches `/json` gets v1, and a v2 overlay
that fetches `/json/v2` gets v2, with no shim involvement.

Overlays that read tosu's **v1** payload shape (`client.gameplay`, `resultsScreen`,
`menu`) work over `/ws` and `/json`. Only the URL is rewritten, never the JSON
body — an overlay that expects v1 field names at `/json/v2` will still be reading
v2.

## Which values are populated when

`play.*` reflects osu!'s live gameplay state, so outside of actual play those
fields are zero. Render them as-is, and do not treat zero as an error.

| Field | Menu / song select | Playing | Results screen |
| --- | --- | --- | --- |
| `beatmap.*`, `play.mods` | populated | populated | populated |
| `play.pp.fc` | **populated** | populated | populated |
| `play.pp.current`, `maxAchieved` | `0` | live | final |
| `play.score`, `combo`, `hits`, `accuracy`, `unstableRate` | `0` | live | final |

`play.pp.fc` is available as soon as a map is loaded, so an overlay can show
"pp if FC" in the menu. Note that mods are read from gameplay state, so before
the first play of a session the FC value is calculated for nomod.

`play.*` is cleared when a different map is loaded, so a previous attempt's
score, combo, hits, and grade never carry over. The beatmap metadata, mods, and
`play.pp.fc` intentionally survive, because they describe the new map.

`play.rank.current` is the grade the player holds right now, which is useful for
colouring an accuracy readout by how the run is going. The provider uses the
legacy letter names, with a **trailing `H` for the silver variant** that osu!
awards when a vision-obscuring mod (Hidden, Flashlight, or Fade In) is active:

| Grade | Meaning |
| --- | --- |
| `X` | gold, perfect |
| `XH` | silver, perfect with HD, FL, or FI |
| `S` / `SH` | gold / silver FC-equivalent |
| `A`, `B`, `C`, `D`, `F` | no silver variant exists |

`SS` and `SSH` are accepted as aliases for `X` and `XH`. An empty grade means
the gameplay state could not be read, so do not substitute a default.

## The bundled examples

| Folder | For | Shows |
| --- | --- | --- |
| `rtosu Example` | single player | map art banner, current pp, pp if FC, combo, UR, grade-coloured accuracy, judgement counts |
| `rtosu Tourney` | tournaments | a ranked column of every player, branded by team colour, with a score bar, set stars and a lead indicator |

Copy either folder to start your own. They are deliberately different shapes,
which is the point: a folder is just a design plus a v2 API consumer, and the
server serves both without knowing what they are.

## Tournaments

In a tournament the root `play` object is **not** populated. Every client has its
own `play` under `tourney.clients[]`, and the v2 schema carries no marker for
which client is the manager. An overlay that wants a single "featured" readout
must therefore pick a client itself; `rtosu Example` uses the lowest `ipcId`,
which is stable for the whole set, and marks that row in the list.

`beatmap` at the root *is* populated in a tournament, so the map header works
either way. A client's own `beatmap` carries only `stats`, not the title or set
id.

More clients than fit is normal, so truncate the list deliberately and show the
total rather than letting a row get sliced in half.

**`clients[].team` is not read from the lobby.** The provider synthesises it by
splitting the ipc list in half, so it is always a clean even/odd split. Two
consequences worth knowing before you rely on it:

- A Free For All lobby is indistinguishable from a symmetric Team vs Team one.
  `rtosu Tourney` therefore takes its layout from `Mode:` in its own
  `metadata.txt`, overridable per source with `?mode=ffa` or `?mode=tvt`, and
  always prints the active mode in the footer.
- An uneven lobby is mislabelled. A 4v2 is reported as 3v3.

Reading the real match type would need a new memory offset for the lobby
settings, which is not implemented.

**Use `tourney.totalScore`, not a sum of `clients[].play.score`.** The server
reads the manager's own team totals, and while `[scoring] enable_mod_multipliers`
is off those totals are exactly the sum of the clients. With it on, each
`clients[].play.score` is weighted by that client's mods — two players on one
team can carry different factors — and the provider replaces `totalScore` with
the sum of the *weighted* scores. An overlay that adds the client rows up itself
would show a bar beside rows that do not add up to it.

If you would rather address the provider directly, the shim exposes it:

```js
window.__rtosu.socket();                 // ws://<host>/websocket/v2
window.__rtosu.preciseSocketUrl;         // '/websocket/v2/precise'
window.__rtosu.jsonUrl;                  // '/json/v2'
window.__rtosu.v1SocketUrl;              // '/ws'
window.__rtosu.v1JsonUrl;                // '/json'
window.__rtosu.backgroundUrl;            // '/files/beatmap/background'
```

`jsonUrl` and `socket` address the v2 payload; `v1JsonUrl` and `v1SocketUrl` address
the gosumemory-compatible v1 payload, on the same paths tosu uses for them.

## `metadata.txt`

Optional. Same format tosu uses, one `Key: Value` per line, with `\n` for line
breaks inside a value.

```
Usecase: obs-overlay
Name: My Overlay
Version: 1.0.0
Author: you
CompatibleWith: tosu
Resolution: 400x90
```

`Name` falls back to the folder name. `authorLinks` is only linked when it
starts with `http://` or `https://`. Recognised keys: `Usecase`, `Name`,
`Version`, `Author`, `CompatibleWith`, `Resolution`, `authorLinks`, `Notes`.

## OBS

1. **Sources** → **+** → **Browser**.
2. Paste the overlay URL.
3. Set the width/height to the overlay's `Resolution`.
4. Leave *Shutdown source when not visible* off if the overlay should keep
   collecting data between scene changes.

The page is served with a dark, transparent-friendly default; a drop-in overlay
brings its own CSS. Refresh the source after editing an overlay, and if it ever
looks stale, clear OBS's browser cache for the source.

## Configuration

Under `[server]` in `config.toml`:

```toml
enable_overlays = true
overlays_dir = "browser_overlays"
```

`overlays_dir` is resolved against the working directory, so a release build can
point at a folder next to the executable. `enable_overlays` requires
`enable_http = true`.

## Notes

- Folders beginning with `.` are ignored, and so is the reserved `__rtosu`
  segment used to serve the shim.
- Requests cannot escape their overlay folder; `..`, absolute paths, and
  Windows drive prefixes are rejected.
- Overlay files are served revalidatable (`Cache-Control: no-cache` plus an
  `ETag` from the file's size and mtime), never cached. Editing an overlay and
  reloading, or refreshing an OBS source, always picks up the change instead of
  silently running a stale copy. HTML additionally sends `Vary: Host`, since the
  injected shim URL embeds the host the page was loaded from.
- **What the file routes expose.** The songs folder is served read-only at both
  `/files/beatmap/{*path}` and `/Songs/{*path}`, and the skin folder at
  `/files/skin/{*path}` — that is what an overlay that loads a map's audio or its
  skin by filename needs, and it is the same set tosu serves. The routes are
  read-only, confined to those two roots by canonicalised path comparison, and
  the background route is a single named file rather than the whole tree.
- If the provider reports no background filename (its memory read is not always
  reliable), the beatmap folder is searched for a conventional
  `background.jpg`/`.png` or a single image in it.
- Keep the vendored `deps/` folder with the overlay. It is served straight from
  disk, so overlays are fully self-contained and work offline from tosu.

## Writing your own

- Load scripts with `defer`, or call your setup from `DOMContentLoaded`. A
  classic script in `<head>` runs before the body exists, so every
  `getElementById` returns `null` and nothing renders even though the socket
  connected.
- Do not swallow render errors silently at 60 Hz. Log the first one; a rendering
  bug is otherwise invisible.
- Do not show a placeholder for the absence of a mod. "NM" is not a mod, it is
  the lack of one, so leave the slot empty.
- Set `white-space: nowrap` on numeric values and give cards holding two numbers
  a smaller font, otherwise a wide value wraps and the row heights drift apart.
- Size your root element to the `Resolution` in `metadata.txt` and keep the
  content inside it, since OBS clips anything that overflows the source box.
- Show idle state honestly. Zeroes from the provider are real, not errors, so
  either display them as-is or gate the element on the osu! state. Do not blank
  out values, since a dash reads as "not working".
