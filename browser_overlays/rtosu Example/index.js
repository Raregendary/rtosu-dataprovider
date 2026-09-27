/* rtosu Example overlay - tosu v2 API consumer.
 *
 * The socket URL below is deliberately the address a stock tosu overlay would
 * hardcode. rtosu-dataprovider injects a compatibility shim into this page that
 * rewrites it onto the origin the page was actually loaded from, so the same
 * file works on a custom port and over the LAN.
 *
 * This script is loaded with `defer` so the DOM exists before init() runs.
 * Copying the file elsewhere without `defer` is still safe: init() falls back
 * to DOMContentLoaded.
 */
(function () {
  'use strict';

  var SOCKET_URL = 'ws://127.0.0.1:24050/websocket/v2';
  var RECONNECT_DELAY = 1000;

  var el = null;
  var reportedError = false;

  /* The shim exposes the address of the server actually serving this page.
   * Prefer it, because a CSS url() cannot be rewritten the way WebSocket,
   * fetch, and XMLHttpRequest calls are. The hardcoded tosu defaults are the
   * fallback for when the page is opened without the shim. */
  function providerUrl(path) {
    if (window.__rtosu && window.__rtosu.origin) return window.__rtosu.origin + path;
    return 'http://127.0.0.1:24050' + path;
  }

  function cacheElements() {
    el = {
      panel: document.querySelector('.panel'),
      banner: document.getElementById('banner'),
      artist: document.getElementById('artist'),
      title: document.getElementById('title'),
      sub: document.getElementById('sub'),
      diff: document.getElementById('diff'),
      pp: document.getElementById('pp'),
      ppfc: document.getElementById('ppfc'),
      combo: document.getElementById('combo'),
      ur: document.getElementById('ur'),
      hit100: document.getElementById('hit100'),
      acc: document.getElementById('acc'),
      accCard: document.querySelector('.hit-acc'),
      hit50: document.getElementById('hit50'),
      hitMiss: document.getElementById('hitMiss'),
      hitMissCard: document.querySelector('.hit-miss'),
      mods: document.getElementById('mods'),
      state: document.getElementById('state'),
      tourney: document.getElementById('tourney'),
      players: document.getElementById('players'),
      clientCount: document.getElementById('clientCount')
    };
  }

  function text(node, value) {
    if (node && node.textContent !== value) node.textContent = value;
  }

  function setStatus(message, connected) {
    if (!el) return;
    text(el.state, message);
    el.state.classList.toggle('offline', !connected);
  }

  function num(value, digits) {
    return Number(value || 0).toFixed(digits);
  }

  function int(value) {
    return Math.round(Number(value) || 0).toLocaleString('en-US');
  }

  function starLevel(stars) {
    return Number(stars && stars.total || 0);
  }

  function starText(stars) {
    var level = starLevel(stars);
    return level > 0 ? level.toFixed(2) + ' stars' : '\u00a0';
  }

  function starColor(stars) {
    var level = starLevel(stars);
    if (level < 3) return '#aab2c4';
    if (level < 5) return '#5cd6a0';
    if (level < 7) return '#ffd166';
    return '#ff7b8a';
  }

  function applyBeatmap(beatmap) {
    // Romanized title and artist, not the Unicode variants, which are the ones
    // most viewers cannot read.
    text(el.artist, beatmap.artist || 'unknown artist');
    text(el.title, beatmap.title || 'no beatmap');

    // Difficulty and mapper under the title.
    var bits = [];
    if (beatmap.version) bits.push('[' + beatmap.version + ']');
    if (beatmap.mapper) bits.push('by ' + beatmap.mapper);
    text(el.sub, bits.length ? bits.join(' \u00b7 ') : '\u00a0');

    text(el.diff, starText(beatmap.stats && beatmap.stats.stars));
    if (el.diff) el.diff.style.color = starColor(beatmap.stats && beatmap.stats.stars);

    // The art is stretched across the header as a cover banner. Only refetch on
    // a map change. The shim rewrites the tosu path to
    // /files/beatmap/background, which serves the loaded map's image from disk.
    var known = Boolean(beatmap.title || beatmap.artist);
    if (el.banner && beatmap.set && known && el.banner.dataset.set !== String(beatmap.set)) {
      el.banner.dataset.set = String(beatmap.set);
      el.banner.style.backgroundImage =
        'url("' + providerUrl('/backgroundImage?mapset=' + beatmap.set + '&t=' + Date.now()) + '")';
    }
  }

  /* osu! rank colours, keyed by the grade string the provider reports.
   *
   * The provider uses the legacy letter names: X for a perfect (SS) and S for
   * an FC-equivalent, each with a trailing H for the silver variant that osu!
   * awards when a vision-obscuring mod (Hidden, Flashlight, or Fade In) is
   * active. SS and SSH are accepted as aliases. A–F have no silver variant. */
  var GRADE_COLORS = {
    X: '#ffe9a3',
    SS: '#ffe9a3',
    XH: '#c3cbd9',
    SSH: '#c3cbd9',
    S: '#ffc44d',
    SH: '#c3cbd9',
    A: '#5cd6a0',
    B: '#b692f0',
    C: '#7fc4e8',
    D: '#a9b4c7',
    F: '#ff7b8a'
  };

  function applyAccuracy(play) {
    text(el.acc, num(play.accuracy, 2) + '%');
    var grade = ((play.rank && play.rank.current) || '').trim().toUpperCase();
    var color = GRADE_COLORS[grade];
    if (el.accCard) {
      el.accCard.style.color = color || '';
      // Without a grade the value falls back to the normal text colour.
      el.accCard.classList.toggle('graded', Boolean(color));
    }
  }

  function applyPlay(play) {
    /* The provider only fills in play state and PP once osu! is playing, so in
     * the menu these read as zero. That is the provider's live value, not a
     * placeholder, so it is shown as-is. */
    var hits = play.hits || {};
    var combo = play.combo || {};
    var judged =
      (hits['0'] || 0) +
      (hits['50'] || 0) +
      (hits['100'] || 0) +
      (hits['300'] || 0) +
      (hits.geki || 0) +
      (hits.katu || 0) +
      (hits.sliderBreaks || 0);
    var active = judged > 0 || (combo.current || 0) > 0;
    var pp = play.pp || {};

    if (el.panel) el.panel.classList.toggle('playing', active);

    text(el.pp, num(pp.current, 2));
    text(el.ppfc, num(pp.fc, 2));
    // Showing "current / max" is only useful while the two differ, so once the
    // run is at its maximum combo just show the number.
    var current = int(combo.current);
    var max = int(combo.max);
    text(el.combo, current === max ? current : current + ' / ' + max);
    text(el.ur, num(play.unstableRate, 2));
    applyAccuracy(play);

    text(el.hit100, int(hits['100']));
    text(el.hit50, int(hits['50']));
    var misses = int(hits['0']);
    text(el.hitMiss, misses);
    // Do not draw attention to a clean run.
    if (el.hitMissCard) el.hitMissCard.classList.toggle('quiet', misses === '0');

    // No mod selected is not a mod, so the slot stays empty.
    var mods = (play.mods && play.mods.name) || '';
    text(el.mods, mods);
    if (el.mods) el.mods.hidden = mods === '';
  }

  function escapeHtml(raw) {
    return String(raw).replace(/[&<>"']/g, function (ch) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch];
    });
  }

  /* Pick the play state the top row should describe.
   *
   * The provider only fills the root `play` object in solo play. In a
   * tournament every client carries its own `play`, and the v2 schema has no
   * marker for which client is the manager, so the featured player is the
   * lowest ipcId, which is stable for the whole set. */
  function featuredPlay(data) {
    var clients = (data.tourney && data.tourney.clients) || [];
    if (clients.length === 0) return data.play || {};
    var featured = clients[0];
    return (featured && featured.play) || {};
  }

  function applyTourney(tourney) {
    var clients = (tourney && tourney.clients) || [];
    el.tourney.hidden = clients.length === 0;
    if (clients.length === 0) {
      document.body.classList.toggle('tournament', false);
      return;
    }

    // More players than fit is normal in a tournament, so say how many there
    // are rather than letting the list look truncated by accident.
    text(el.clientCount, '(' + clients.length + ')');

    // The hit counts are redundant once a tournament is running, so the client
    // list takes that space instead of overflowing the panel.
    document.body.classList.toggle('tournament', true);

    var html = clients
      .map(function (client, index) {
        var play = client.play || {};
        var name = (client.user && client.user.name) || 'unknown';
        // Mark the client the top row is describing, since a tournament has no
        // single "player" and the root play object is empty.
        var marker = index === 0 ? '<span class="featured">&#9656;</span>' : '';
        return (
          '<div class="player">' +
          '<span class="name">' + marker + escapeHtml(name) + '</span>' +
          '<span>' + num(play.accuracy, 2) + '%</span>' +
          '<span>' + int(play.score) + '</span>' +
          '</div>'
        );
      })
      .join('');

    if (el.players.innerHTML !== html) el.players.innerHTML = html;
  }

  function render(data) {
    applyBeatmap(data.beatmap || {});
    applyPlay(featuredPlay(data));
    applyTourney(data.tourney);

    var status = [data.client, data.state && data.state.name].filter(Boolean).join(' \u00b7 ');
    setStatus(status || 'connected', true);
  }

  function connect() {
    var socket;
    try {
      socket = new WebSocket(SOCKET_URL);
    } catch (err) {
      setStatus('connection failed', false);
      setTimeout(connect, RECONNECT_DELAY);
      return;
    }

    socket.onopen = function () {
      setStatus('connected', true);
    };

    socket.onmessage = function (event) {
      if (typeof event.data !== 'string' || !el) return;
      try {
        render(JSON.parse(event.data));
      } catch (err) {
        // A bad frame must not kill the stream, but a rendering bug would
        // otherwise be invisible, so report it once rather than 60x a second.
        if (!reportedError) {
          reportedError = true;
          if (window.console && console.error) console.error('overlay render failed', err);
        }
      }
    };

    socket.onclose = function () {
      setStatus('reconnecting\u2026', false);
      setTimeout(connect, RECONNECT_DELAY);
    };

    socket.onerror = function () {
      if (socket.readyState !== WebSocket.OPEN) socket.close();
    };
  }

  function init() {
    cacheElements();
    if (!el) return;
    setStatus('connecting\u2026', false);
    connect();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
