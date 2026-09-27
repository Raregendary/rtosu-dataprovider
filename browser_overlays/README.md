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
| `ws://127.0.0.1:24050/ws` | `ws://<page-host>/websocket/v2` |
| `ws://<host>:<port>` | `ws://<page-host>/websocket/v2` |
| `http://127.0.0.1:24050/backgroundImage?mapset=1` | `/files/beatmap/background` |
| `http://127.0.0.1:24050/Songs/...` | `/files/beatmap/...` |

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

Overlays that read tosu's **v1** payload shape (for example `tourney.ipcClients`
or `client.gameplay`) will connect but see v2 field names. Only the URL is
rewritten, not the JSON body.

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

## Tournaments

In a tournament the root `play` object is **not** populated. Every client has
its own `play` under `tourney.clients[]`, and the v2 schema carries no marker for
which client is the manager. An overlay that wants a single "featured" readout
must therefore pick a client itself; the bundled example uses the lowest
`ipcId`, which is stable for the whole set, and marks that row in the list.

`beatmap` at the root *is* populated in a tournament, so the map header works
either way. A client's own `beatmap` carries only `stats`, not the title or set
id.

More clients than fit is normal, so truncate the list deliberately and show the
total rather than letting a row get sliced in half.

If you would rather address the provider directly, the shim exposes it:

```js
window.__rtosu.socket();                 // ws://<host>/websocket/v2
window.__rtosu.preciseSocketUrl;         // '/websocket/v2/precise'
window.__rtosu.jsonUrl;                  // '/json/v2'
window.__rtosu.backgroundUrl;            // '/files/beatmap/background'
```

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
- Only the current beatmap background is exposed. The arbitrary osu! songs
  folder is not browsable.
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
