/* rtosu Tourney overlay - tosu v2 API consumer.
 *
 * One column of players stacked top to bottom. In Team vs Team each row is
 * branded with its team colour and ranked within its own team, with a score bar
 * across the top carrying the set score, the set stars, and who is leading. In
 * Free For All there are no teams, so the same column becomes a single ranking
 * with a summary strip.
 *
 * The lobby mode cannot be read from the packet. The provider synthesises
 * clients[].team by splitting the ipc list in half rather than reading the
 * lobby, so an FFA and a symmetric TvT are indistinguishable here. It is
 * therefore taken from metadata.txt ("Mode: ffa" / "Mode: tvt"), overridable
 * per source with ?mode=ffa or ?mode=tvt, and always shown in the footer so a
 * wrong choice is visible rather than silent.
 *
 * Loaded with `defer`; init() also falls back to DOMContentLoaded.
 */
(function () {
  'use strict';

  var SOCKET_URL = 'ws://127.0.0.1:24050/websocket/v2';
  var RECONNECT_DELAY = 1000;

  /* Used when metadata.txt has no Mode line. */
  var DEFAULT_MODE = 'tvt';

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

  var el = null;
  var reportedError = false;
  /* Null until the first applyMode, so the initial layout is always applied
   * even when the resolved mode equals the default. */
  var mode = null;

  function providerUrl(path) {
    if (window.__rtosu && window.__rtosu.origin) return window.__rtosu.origin + path;
    return 'http://127.0.0.1:24050' + path;
  }

  function cacheElements() {
    el = {
      banner: document.getElementById('banner'),
      artist: document.getElementById('artist'),
      title: document.getElementById('title'),
      sub: document.getElementById('sub'),
      stars: document.getElementById('stars'),      teambar: document.getElementById('teambar'),
      leftName: document.getElementById('leftName'),
      rightName: document.getElementById('rightName'),
      leftScore: document.getElementById('leftScore'),
      rightScore: document.getElementById('rightScore'),
      leftPoints: document.getElementById('leftPoints'),
      rightPoints: document.getElementById('rightPoints'),
      lead: document.getElementById('lead'),
      format: document.getElementById('format'),
      summary: document.getElementById('summary'),
      leadName: document.getElementById('leadName'),
      playerCount: document.getElementById('playerCount'),
      bestPp: document.getElementById('bestPp'),
      colheads: document.getElementById('colheads'),
      colAll: document.getElementById('colAll'),
      state: document.getElementById('state'),
      mode: document.getElementById('mode'),
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

  function escapeHtml(raw) {
    return String(raw).replace(/[&<>"']/g, function (ch) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch];
    });
  }

  function starLevel(stars) {
    return Number((stars && stars.total) || 0);
  }

  function starColor(stars) {
    var level = starLevel(stars);
    if (level < 3) return '#aab2c4';
    if (level < 5) return '#5cd6a0';
    if (level < 7) return '#ffd166';
    return '#ff7b8a';
  }

  /* --- mode --- */

  function normaliseMode(raw) {
    return String(raw || '').trim().toLowerCase() === 'ffa' ? 'ffa' : 'tvt';
  }

  /* The query string wins so one folder can serve both layouts in OBS. */
  function modeFromQuery() {
    try {
      var requested = new URLSearchParams(window.location.search).get('mode');
      return requested ? normaliseMode(requested) : null;
    } catch (err) {
      return null;
    }
  }

  /* metadata.txt is served from the same folder, so the mode can live beside
   * the overlay instead of inside the script. */
  function loadModeFromMetadata() {
    var forced = modeFromQuery();
    if (forced) return Promise.resolve(forced);
    if (typeof fetch !== 'function') return Promise.resolve(DEFAULT_MODE);
    var folder = window.location.pathname.replace(/\/[^/]*$/, '');
    return fetch(providerUrl(folder + '/metadata.txt'))
      .then(function (response) {
        return response.ok ? response.text() : '';
      })
      .then(function (content) {
        var match = /(?:^|\n)\s*Mode\s*:\s*(\S+)/i.exec(content);
        return match ? normaliseMode(match[1]) : DEFAULT_MODE;
      })
      .catch(function () {
        return DEFAULT_MODE;
      });
  }

  function applyMode(next) {
    mode = next;
    var isFfa = mode === 'ffa';
    document.body.classList.toggle('ffa', isFfa);
    el.teambar.hidden = isFfa;
    el.summary.hidden = !isFfa;
    text(el.mode, isFfa ? 'free for all' : 'team vs team');
  }

  /* --- header --- */

  function applyBeatmap(beatmap) {
    // Romanized title and artist, not the Unicode variants, which are the ones
    // most viewers cannot read.
    var title = beatmap.title || 'no beatmap';
    text(el.artist, beatmap.artist || 'unknown artist');
    text(el.title, title);

    // Difficulty and mapper under the title.
    var bits = [];
    if (beatmap.version) bits.push('[' + beatmap.version + ']');
    if (beatmap.mapper) bits.push('by ' + beatmap.mapper);
    text(el.sub, bits.length ? bits.join(' \u00b7 ') : '\u00a0');

    var stars = beatmap.stats && beatmap.stats.stars;
    text(el.stars, starLevel(stars) > 0 ? starLevel(stars).toFixed(2) + ' \u2605' : '\u00a0');
    if (el.stars) el.stars.style.color = starColor(stars);

    // Only ask for art when the map is actually identified. A tournament client
    // often selects a map it has not downloaded, in which case the title is
    // empty and a background request is a guaranteed miss.
    var known = Boolean(beatmap.title || beatmap.artist);
    if (!el.banner) return;
    if (!known || !beatmap.set) {
      el.banner.dataset.set = '';
      el.banner.style.backgroundImage = '';
      return;
    }
    if (el.banner.dataset.set !== String(beatmap.set)) {
      el.banner.dataset.set = String(beatmap.set);
      el.banner.style.backgroundImage =
        'url("' + providerUrl('/backgroundImage?mapset=' + beatmap.set + '&t=' + Date.now()) + '")';
    }
  }

  /* --- ranking --- */

  /* Best performing first. Score decides; accuracy then name break ties so the
   * order never jitters between equal scores. */
  function nameOf(client) {
    return (client.user && client.user.name) || '';
  }

  /* A slot with nobody logged in is not a player, so it is hidden rather than
   * shown as a nameless row. */
  function isAnonymous(client) {
    return nameOf(client) === '';
  }

  function byScore(a, b) {
    var sa = Number((a.play && a.play.score) || 0);
    var sb = Number((b.play && b.play.score) || 0);
    if (sb !== sa) return sb - sa;
    var aa = Number((a.play && a.play.accuracy) || 0);
    var ab = Number((b.play && b.play.accuracy) || 0);
    if (ab !== aa) return ab - aa;
    return nameOf(a).localeCompare(nameOf(b));
  }

  /* Combo collapses to one number once the run reaches its own maximum, the
   * same rule the solo overlay uses. */
  function comboText(combo) {
    var current = Math.round(Number(combo.current) || 0);
    var max = Math.round(Number(combo.max) || 0);
    if (current === max) return int(current) + 'x';
    return int(current) + '/' + int(max);
  }

  function isIdle(client) {
    var play = client.play || {};
    var hits = play.hits || {};
    var judged =
      (hits['0'] || 0) + (hits['50'] || 0) + (hits['100'] || 0) + (hits['300'] || 0);
    return judged === 0 && !(play.combo && play.combo.current) && !(play.score > 0);
  }

  function rowHtml(client, position, team) {
    var play = client.play || {};
    var hits = play.hits || {};
    var judged =
      (hits['0'] || 0) + (hits['50'] || 0) + (hits['100'] || 0) + (hits['300'] || 0);
    var idle = isIdle(client);
    var name = nameOf(client);
    var grade = String((play.rank && play.rank.current) || '').toUpperCase();
    var gradeColor = GRADE_COLORS[grade] || '#6d7486';
    var combo = play.combo || {};
    var pp = play.pp || {};

    var classes = ['row'];
    if (team) classes.push(team);
    if (idle) classes.push('idle');
    if (!team && position === 1 && !idle) classes.push('leader');

    var cells =
      '<span class="cell acc">' + (judged > 0 ? num(play.accuracy, 1) + '%' : '\u2014') + '</span>' +
      '<span class="cell combo">' + (judged > 0 ? comboText(combo) : '\u2014') + '</span>' +
      '<span class="cell pp">' + (judged > 0 ? num(pp.current, 1) + 'pp' : '\u2014') + '</span>' +
      '<span class="cell score">' + int(play.score) + '</span>';

    return (
      '<div class="' + classes.join(' ') + '">' +
      '<span class="rank">' + position + '</span>' +
      '<span class="tag">' + (team ? team.charAt(0).toUpperCase() : '') + '</span>' +
      '<span class="player">' + escapeHtml(name) + '</span>' +
      '<span class="grade" style="color:' + gradeColor + '">' + escapeHtml(grade || '\u2014') + '</span>' +
      cells +
      '</div>'
    );
  }

  function applyBoard(tourney) {
    // Anonymous slots are dropped before ranking, so positions only count real
    // players.
    var clients = ((tourney && tourney.clients) || []).filter(function (client) {
      return !isAnonymous(client);
    });

    if (clients.length === 0) {
      el.colAll.innerHTML =
        '<div class="row idle"><span class="rank"></span><span class="tag"></span>' +
        '<span class="player">no players</span></div>';
      return;
    }

    var ranked = clients.slice().sort(byScore);
    var html = '';

    if (mode === 'ffa') {
      html = ranked
        .map(function (client, index) {
          return rowHtml(client, index + 1, null);
        })
        .join('');
    } else {
      // Stacked in one column, but ranked inside each team so the number on a
      // row always means the same thing.
      var perTeam = { left: 0, right: 0 };
      html = ranked
        .map(function (client) {
          var team = client.team === 'right' ? 'right' : 'left';
          perTeam[team] += 1;
          return rowHtml(client, perTeam[team], team);
        })
        .join('');
    }

    if (el.colAll.innerHTML !== html) el.colAll.innerHTML = html;
  }

  /* --- score bar --- */

  function applyTeams(tourney, hasClients) {
    var total = tourney.totalScore || {};
    var points = tourney.points || {};
    var team = tourney.team || {};

    // With nobody attached the manager may still hold the last set's totals, so
    // an empty lobby must not keep claiming a scoreline.
    var left = hasClients ? Number(total.left) || 0 : 0;
    var right = hasClients ? Number(total.right) || 0 : 0;
    var leftPoints = hasClients ? Number(points.left) || 0 : 0;
    var rightPoints = hasClients ? Number(points.right) || 0 : 0;

    text(el.leftScore, int(left));
    text(el.rightScore, int(right));
    text(el.leftName, team.left || 'left');
    text(el.rightName, team.right || 'right');
    el.leftPoints.innerHTML = '<b>' + int(leftPoints) + '</b> set stars';
    el.rightPoints.innerHTML = '<b>' + int(rightPoints) + '</b> set stars';

    // Who is leading, by how much, in that side's colour.
    var diff = left - right;
    el.lead.classList.remove('left', 'right', 'tie');
    if (!hasClients || diff === 0) {
      text(el.lead, hasClients ? 'tied' : 'no score');
      el.lead.classList.add('tie');
    } else if (diff > 0) {
      text(el.lead, 'left +' + int(diff));
      el.lead.classList.add('left');
    } else {
      text(el.lead, 'right +' + int(-diff));
      el.lead.classList.add('right');
    }

    // The v2 schema names this `bestOF`, not `bestOf`.
    var bestOf = Number(tourney.bestOF !== undefined ? tourney.bestOF : tourney.bestOf) || 0;
    text(el.format, bestOf > 0 ? 'best of ' + bestOf : 'lobby');
  }

  function applySummary(clients) {
    var best = null;
    clients.forEach(function (client) {
      if (isIdle(client)) return;
      if (!best || byScore(client, best) < 0) best = client;
    });
    text(el.leadName, best ? nameOf(best) : '\u2014');    text(el.playerCount, clients.length);
    var topPp = 0;
    clients.forEach(function (client) {
      var pp = (client.play && client.play.pp && client.play.pp.current) || 0;
      if (pp > topPp) topPp = pp;
    });
    text(el.bestPp, num(topPp, 2));
  }

  function render(data) {
    applyBeatmap(data.beatmap || {});
    var tourney = data.tourney || {};
    var clients = (tourney.clients || []).filter(function (client) {
      return !isAnonymous(client);
    });
    var hasClients = clients.length > 0;

    if (mode === 'ffa') {
      applySummary(clients);
    } else {
      applyTeams(tourney, hasClients);
    }
    applyBoard(tourney);

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
    loadModeFromMetadata().then(function (chosen) {
      applyMode(chosen);
      connect();
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
